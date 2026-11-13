//! Native presentation values used after typed provider conversion.
//!
//! Every adapter is lenient about optional presentation fields and strict about
//! what would make a result unusable: an item that cannot be read is dropped, and
//! the rest of the answer survives. Unopened-file positions retain their units,
//! together with the encoding they count in; a caller with the text of the
//! document turns them into byte offsets with [`Span::bytes`].
use super::capabilities::PositionEncoding;
use super::position::{LineIndex, Position};
use serde_json::{Value, json};
use std::ops::Range;

/// A stretch of a document in the server's own terms.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Span {
    pub start: Position,
    pub end: Position,
    pub encoding: PositionEncoding,
}

impl Span {
    /// The bytes of `text` this span covers. A span that is backwards or outside
    /// the text is clamped, never rejected: a stale answer still lands somewhere
    /// sensible, and callers drop answers whose document has changed.
    pub fn bytes(&self, text: &str) -> Range<usize> {
        let index = LineIndex::new(text);
        let start = index.offset(self.start, self.encoding);
        let end = index.offset(self.end, self.encoding);
        start.min(end)..start.max(end)
    }

    /// The span of `bytes` in `text`, counted in `encoding`.
    pub fn of(text: &str, bytes: Range<usize>, encoding: PositionEncoding) -> Self {
        let index = LineIndex::new(text);
        Self {
            start: index.position(bytes.start, encoding),
            end: index.position(bytes.end, encoding),
            encoding,
        }
    }

    pub fn json(self) -> Value {
        json!({
            "start": { "line": self.start.line, "character": self.start.character },
            "end": { "line": self.end.line, "character": self.end.character },
        })
    }
}

pub fn position(value: &Value) -> Option<Position> {
    Some(Position {
        line: u32::try_from(value.get("line")?.as_u64()?).ok()?,
        character: u32::try_from(value.get("character")?.as_u64()?).ok()?,
    })
}

pub fn span(value: &Value, encoding: PositionEncoding) -> Option<Span> {
    let encoding = match value.get("x-loog-column-unit").and_then(Value::as_str) {
        Some("utf8") => PositionEncoding::Utf8,
        Some("utf16") => PositionEncoding::Utf16,
        _ => encoding,
    };
    Some(Span {
        start: position(value.get("start")?)?,
        end: position(value.get("end")?)?,
        encoding,
    })
}

fn text(value: &Value, key: &str) -> Option<String> {
    value.get(key)?.as_str().map(str::to_owned)
}

fn items(value: &Value) -> impl Iterator<Item = &Value> {
    value.as_array().into_iter().flatten()
}

// ---- documentation -------------------------------------------------------------

/// Markdown for any of the protocol's ways to say "text with a format": a plain
/// string, `MarkupContent`, a `MarkedString` code block, or a list of them.
pub fn markup(value: &Value) -> Option<crate::features::markdown::content::Markup> {
    match value {
        Value::String(text) => Some(crate::features::markdown::content::Markup::markdown(
            text.clone(),
        )),
        Value::Object(object) => {
            let body = object.get("value")?.as_str()?;
            match (object.get("kind"), object.get("language")) {
                // `plaintext` content is escaped only by being shown as it is; callers
                // that render Markdown should treat the whole string as Markdown.
                (Some(kind), _) => Some(if kind.as_str() == Some("plaintext") {
                    crate::features::markdown::content::Markup::plain(body)
                } else {
                    crate::features::markdown::content::Markup::markdown(body)
                }),
                (None, Some(language)) => Some(crate::features::markdown::content::Markup::code(
                    body,
                    language.as_str().unwrap_or_default(),
                )),
                (None, None) => Some(crate::features::markdown::content::Markup::markdown(body)),
            }
        }
        Value::Array(parts) => {
            let parts = parts.iter().filter_map(markup).collect::<Vec<_>>();
            crate::features::markdown::content::Markup::join(&parts)
        }
        _ => None,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hover {
    pub markdown: crate::features::markdown::content::Markup,
    pub span: Option<Span>,
}

pub fn hover(value: &Value, encoding: PositionEncoding) -> Option<Hover> {
    let markdown = markup(value.get("contents")?)?;
    if markdown.trim().is_empty() {
        return None;
    }
    Some(Hover {
        markdown,
        span: value.get("range").and_then(|r| span(r, encoding)),
    })
}

// ---- locations -----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Location {
    pub resource: loog_workspace_api::ResourceRef,
    /// What to select at the target (the name for a link, the range otherwise).
    pub span: Span,
    /// The whole target (a function with its body), when the server says.
    pub full: Option<Span>,
    /// The text at the origin the link was computed from.
    pub origin: Option<Span>,
}

/// `Location | Location[] | LocationLink[] | null`.
pub fn locations(value: &Value, encoding: PositionEncoding) -> Vec<Location> {
    let one = |item: &Value| -> Option<Location> {
        if let Some(resource) = item.get("resource") {
            return Some(Location {
                resource: serde_json::from_value(resource.clone()).ok()?,
                span: span(item.get("range")?, encoding)?,
                full: None,
                origin: None,
            });
        }
        // COMPAT(native-location-uris): Existing local language replies use file
        // URIs. Keep decoding those as native resources; provided replies carry
        // explicit ResourceRef instead. No removal version is scheduled.
        if let Some(target) = item.get("targetUri") {
            let full = span(item.get("targetRange")?, encoding)?;
            return Some(Location {
                resource: super::uri::file_path(target.as_str()?)?.into(),
                span: item
                    .get("targetSelectionRange")
                    .and_then(|r| span(r, encoding))
                    .unwrap_or(full),
                full: Some(full),
                origin: item
                    .get("originSelectionRange")
                    .and_then(|r| span(r, encoding)),
            });
        }
        Some(Location {
            resource: super::uri::file_path(item.get("uri")?.as_str()?)?.into(),
            span: span(item.get("range")?, encoding)?,
            full: None,
            origin: None,
        })
    };
    match value {
        Value::Array(list) => list.iter().filter_map(one).collect(),
        Value::Object(_) => one(value).into_iter().collect(),
        _ => Vec::new(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HighlightKind {
    Text,
    Read,
    Write,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Highlight {
    pub span: Span,
    pub kind: HighlightKind,
}

pub fn highlights(value: &Value, encoding: PositionEncoding) -> Vec<Highlight> {
    items(value)
        .filter_map(|item| {
            Some(Highlight {
                span: span(item.get("range")?, encoding)?,
                kind: match item.get("kind").and_then(Value::as_u64) {
                    Some(2) => HighlightKind::Read,
                    Some(3) => HighlightKind::Write,
                    _ => HighlightKind::Text,
                },
            })
        })
        .collect()
}

// ---- symbols -------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Symbol {
    pub name: String,
    pub detail: Option<String>,
    /// The protocol's `SymbolKind` number.
    pub kind: u32,
    pub deprecated: bool,
    /// Present for workspace symbols and flat document symbols.
    pub uri: Option<String>,
    pub container: Option<String>,
    pub full: Span,
    /// What to select: the name.
    pub selection: Span,
    pub children: Vec<Symbol>,
}

fn symbol(item: &Value, encoding: PositionEncoding) -> Option<Symbol> {
    let name = text(item, "name")?;
    let kind = u32::try_from(item.get("kind")?.as_u64()?).ok()?;
    let deprecated = item.get("deprecated").and_then(Value::as_bool) == Some(true)
        || item
            .get("tags")
            .and_then(Value::as_array)
            .is_some_and(|tags| tags.iter().any(|t| t.as_u64() == Some(1)));
    if let Some(range) = item.get("range") {
        // DocumentSymbol.
        let full = span(range, encoding)?;
        return Some(Symbol {
            name,
            detail: text(item, "detail"),
            kind,
            deprecated,
            uri: None,
            container: None,
            selection: item
                .get("selectionRange")
                .and_then(|r| span(r, encoding))
                .unwrap_or(full),
            full,
            children: items(item.get("children").unwrap_or(&Value::Null))
                .filter_map(|child| symbol(child, encoding))
                .collect(),
        });
    }
    // SymbolInformation or WorkspaceSymbol: the location holds a range or only a uri.
    let location = item.get("location")?;
    let uri = text(location, "uri")?;
    let full = location
        .get("range")
        .and_then(|r| span(r, encoding))
        .unwrap_or(Span {
            start: Position::default(),
            end: Position::default(),
            encoding,
        });
    Some(Symbol {
        name,
        detail: text(item, "detail"),
        kind,
        deprecated,
        uri: Some(uri),
        container: text(item, "containerName").filter(|c| !c.is_empty()),
        full,
        selection: full,
        children: Vec::new(),
    })
}

pub fn symbols(value: &Value, encoding: PositionEncoding) -> Vec<Symbol> {
    items(value)
        .filter_map(|item| symbol(item, encoding))
        .collect()
}

// ---- edits ---------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextEdit {
    pub span: Span,
    pub new_text: String,
}

fn edit(item: &Value, encoding: PositionEncoding) -> Option<TextEdit> {
    Some(TextEdit {
        span: span(item.get("range")?, encoding)?,
        new_text: text(item, "newText")?,
    })
}

pub fn text_edits(value: &Value, encoding: PositionEncoding) -> Vec<TextEdit> {
    items(value).filter_map(|e| edit(e, encoding)).collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentEdit {
    pub uri: String,
    /// The version of the document the edits were computed for, if the server knows.
    pub version: Option<i64>,
    pub edits: Vec<TextEdit>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Change {
    Edit(DocumentEdit),
    ResourceText(loog_workspace_api::session::ResourceDocumentEdit),
    Create {
        uri: String,
        overwrite: bool,
        ignore_if_exists: bool,
    },
    Rename {
        from: String,
        to: String,
        overwrite: bool,
        ignore_if_exists: bool,
    },
    Delete {
        uri: String,
        recursive: bool,
        ignore_if_not_exists: bool,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WorkspaceEdit {
    pub changes: Vec<Change>,
}

/// A `WorkspaceEdit`, or why it cannot be applied: an unreadable piece would
/// leave the edit half done, so the whole edit is refused.
pub fn workspace_edit(value: &Value, encoding: PositionEncoding) -> Result<WorkspaceEdit, String> {
    let mut changes = Vec::new();
    if let Some(resources) = value.get("resourceChanges") {
        if value.get("documentChanges").is_some() || value.get("changes").is_some() {
            return Err("Mixed native and resource edit representations".into());
        }
        let documents: Vec<loog_workspace_api::session::ResourceDocumentEdit> =
            serde_json::from_value(resources.clone()).map_err(|_| "Invalid resource text edits")?;
        return Ok(WorkspaceEdit {
            changes: documents.into_iter().map(Change::ResourceText).collect(),
        });
    }
    if let Some(list) = value.get("documentChanges").and_then(Value::as_array) {
        for item in list {
            let flag = |key: &str| {
                item.get("options")
                    .and_then(|o| o.get(key))
                    .and_then(Value::as_bool)
                    .unwrap_or(false)
            };
            match item.get("kind").and_then(Value::as_str) {
                Some("create") => changes.push(Change::Create {
                    uri: text(item, "uri").ok_or("a create without a uri")?,
                    overwrite: flag("overwrite"),
                    ignore_if_exists: flag("ignoreIfExists"),
                }),
                Some("rename") => changes.push(Change::Rename {
                    from: text(item, "oldUri").ok_or("a rename without oldUri")?,
                    to: text(item, "newUri").ok_or("a rename without newUri")?,
                    overwrite: flag("overwrite"),
                    ignore_if_exists: flag("ignoreIfExists"),
                }),
                Some("delete") => changes.push(Change::Delete {
                    uri: text(item, "uri").ok_or("a delete without a uri")?,
                    recursive: flag("recursive"),
                    ignore_if_not_exists: flag("ignoreIfNotExists"),
                }),
                Some(other) => return Err(format!("unknown resource operation {other:?}")),
                None => {
                    let document = item
                        .get("textDocument")
                        .ok_or("an edit without a document")?;
                    let edits = item
                        .get("edits")
                        .and_then(Value::as_array)
                        .ok_or("an edit without edits")?;
                    let parsed: Vec<TextEdit> =
                        edits.iter().filter_map(|e| edit(e, encoding)).collect();
                    if parsed.len() != edits.len() {
                        return Err("an edit that cannot be read".to_owned());
                    }
                    changes.push(Change::Edit(DocumentEdit {
                        uri: text(document, "uri").ok_or("a document without a uri")?,
                        version: document.get("version").and_then(Value::as_i64),
                        edits: parsed,
                    }));
                }
            }
        }
    } else if let Some(map) = value.get("changes").and_then(Value::as_object) {
        for (uri, edits) in map {
            let list = edits.as_array().ok_or("edits that are not a list")?;
            let parsed: Vec<TextEdit> = list.iter().filter_map(|e| edit(e, encoding)).collect();
            if parsed.len() != list.len() {
                return Err("an edit that cannot be read".to_owned());
            }
            changes.push(Change::Edit(DocumentEdit {
                uri: uri.clone(),
                version: None,
                edits: parsed,
            }));
        }
    }
    Ok(WorkspaceEdit { changes })
}

// ---- completion ----------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionEdit {
    /// Replaces this when the person is at the start of the word.
    pub insert: Span,
    /// Replaces this when the person asked to replace the whole word.
    pub replace: Span,
    pub new_text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletionItem {
    pub label: String,
    /// Text shown after the label (signature or source).
    pub label_detail: Option<String>,
    pub kind: Option<u32>,
    pub detail: Option<String>,
    pub documentation: Option<crate::features::markdown::content::Markup>,
    pub deprecated: bool,
    pub preselect: bool,
    pub sort_text: Option<String>,
    pub filter_text: Option<String>,
    pub insert_text: Option<String>,
    pub snippet: bool,
    /// Insert as is, without adjusting the indentation of later lines.
    pub as_is: bool,
    pub edit: Option<CompletionEdit>,
    pub additional_edits: Vec<TextEdit>,
    pub commit_characters: Vec<String>,
    pub command: Option<Command>,
    /// The item exactly as the server sent it, for `completionItem/resolve`.
    pub raw: Value,
}

impl CompletionItem {
    /// The text the person is matching against.
    pub fn filter(&self) -> &str {
        self.filter_text.as_deref().unwrap_or(&self.label)
    }

    /// What is inserted when nothing more specific is said.
    pub fn plain_insert(&self) -> &str {
        self.insert_text.as_deref().unwrap_or(&self.label)
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompletionList {
    pub items: Vec<CompletionItem>,
    /// Typing more characters may produce other items: ask again.
    pub incomplete: bool,
}

pub fn completion_item(item: &Value, encoding: PositionEncoding) -> Option<CompletionItem> {
    let label = text(item, "label")?;
    let edit = item.get("textEdit").and_then(|e| {
        let new_text = text(e, "newText")?;
        match e.get("range") {
            Some(range) => {
                let range = span(range, encoding)?;
                Some(CompletionEdit {
                    insert: range,
                    replace: range,
                    new_text,
                })
            }
            None => Some(CompletionEdit {
                insert: span(e.get("insert")?, encoding)?,
                replace: span(e.get("replace")?, encoding)?,
                new_text,
            }),
        }
    });
    Some(CompletionItem {
        label_detail: item.get("labelDetails").and_then(|d| {
            let joined = format!(
                "{}{}",
                text(d, "detail").unwrap_or_default(),
                text(d, "description")
                    .map(|d| format!(" {d}"))
                    .unwrap_or_default()
            );
            (!joined.trim().is_empty()).then_some(joined)
        }),
        kind: item
            .get("kind")
            .and_then(Value::as_u64)
            .and_then(|k| u32::try_from(k).ok()),
        detail: text(item, "detail"),
        documentation: item.get("documentation").and_then(markup),
        deprecated: item.get("deprecated").and_then(Value::as_bool) == Some(true)
            || item
                .get("tags")
                .and_then(Value::as_array)
                .is_some_and(|tags| tags.iter().any(|t| t.as_u64() == Some(1))),
        preselect: item.get("preselect").and_then(Value::as_bool) == Some(true),
        sort_text: text(item, "sortText"),
        filter_text: text(item, "filterText"),
        insert_text: text(item, "insertText"),
        snippet: item.get("insertTextFormat").and_then(Value::as_u64) == Some(2),
        as_is: item.get("insertTextMode").and_then(Value::as_u64) == Some(1),
        edit,
        additional_edits: text_edits(
            item.get("additionalTextEdits").unwrap_or(&Value::Null),
            encoding,
        ),
        commit_characters: item
            .get("commitCharacters")
            .and_then(Value::as_array)
            .map(|c| {
                c.iter()
                    .filter_map(|c| c.as_str().map(str::to_owned))
                    .collect()
            })
            .unwrap_or_default(),
        command: item.get("command").and_then(command),
        raw: item.clone(),
        label,
    })
}

/// `CompletionItem[] | CompletionList | null`.
pub fn completion(value: &Value, encoding: PositionEncoding) -> CompletionList {
    let (list, incomplete) = match value {
        Value::Array(_) => (value, false),
        Value::Object(object) => (
            object.get("items").unwrap_or(&Value::Null),
            object.get("isIncomplete").and_then(Value::as_bool) == Some(true),
        ),
        _ => return CompletionList::default(),
    };
    // List-wide defaults a server may state once for all its items.
    let defaults = value.get("itemDefaults");
    let items = items(list)
        .filter_map(|item| {
            let mut item = item.clone();
            if let (Some(defaults), Some(object)) = (defaults, item.as_object_mut()) {
                for (from, to) in [
                    ("commitCharacters", "commitCharacters"),
                    ("insertTextFormat", "insertTextFormat"),
                    ("insertTextMode", "insertTextMode"),
                ] {
                    if let Some(default) = defaults.get(from) {
                        object.entry(to).or_insert_with(|| default.clone());
                    }
                }
                if let Some(range) = defaults.get("editRange") {
                    if object.get("textEdit").is_none() {
                        let new_text = object
                            .get("textEditText")
                            .or_else(|| object.get("insertText"))
                            .or_else(|| object.get("label"))
                            .cloned();
                        if let Some(new_text) = new_text {
                            let edit = if range.get("insert").is_some() {
                                json!({ "newText": new_text, "insert": range["insert"], "replace": range["replace"] })
                            } else {
                                json!({ "newText": new_text, "range": range })
                            };
                            object.insert("textEdit".into(), edit);
                        }
                    }
                }
            }
            completion_item(&item, encoding)
        })
        .collect();
    CompletionList { items, incomplete }
}

// ---- signature help ------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Parameter {
    /// The part of the signature label this parameter covers, as character
    /// offsets into the label.
    pub label: Range<usize>,
    pub documentation: Option<crate::features::markdown::content::Markup>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Signature {
    pub label: String,
    pub documentation: Option<crate::features::markdown::content::Markup>,
    pub parameters: Vec<Parameter>,
    pub active_parameter: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignatureHelp {
    pub signatures: Vec<Signature>,
    pub active_signature: usize,
    pub active_parameter: Option<usize>,
}

pub fn signature_help(value: &Value) -> Option<SignatureHelp> {
    let signatures: Vec<Signature> = items(value.get("signatures")?)
        .filter_map(|item| {
            let label = text(item, "label")?;
            let parameters = items(item.get("parameters").unwrap_or(&Value::Null))
                .filter_map(|p| {
                    let range = match p.get("label")? {
                        Value::String(part) => {
                            let at = label.find(part.as_str())?;
                            let start = label[..at].chars().count();
                            start..start + part.chars().count()
                        }
                        Value::Array(pair) if pair.len() == 2 => {
                            // Offsets are UTF-16 code units of the label.
                            let (a, b) = (pair[0].as_u64()? as usize, pair[1].as_u64()? as usize);
                            utf16_to_chars(&label, a)..utf16_to_chars(&label, b)
                        }
                        _ => return None,
                    };
                    Some(Parameter {
                        label: range,
                        documentation: p.get("documentation").and_then(markup),
                    })
                })
                .collect();
            Some(Signature {
                label,
                documentation: item.get("documentation").and_then(markup),
                parameters,
                active_parameter: item
                    .get("activeParameter")
                    .and_then(Value::as_u64)
                    .map(|n| n as usize),
            })
        })
        .collect();
    if signatures.is_empty() {
        return None;
    }
    let active_signature = value
        .get("activeSignature")
        .and_then(Value::as_u64)
        .filter(|n| *n < signatures.len() as u64)
        .map_or(0, |n| n as usize);
    Some(SignatureHelp {
        active_signature,
        active_parameter: value
            .get("activeParameter")
            .and_then(Value::as_u64)
            .map(|n| n as usize),
        signatures,
    })
}

fn utf16_to_chars(text: &str, units: usize) -> usize {
    let mut seen = 0;
    for (chars, ch) in text.chars().enumerate() {
        if seen >= units {
            return chars;
        }
        seen += ch.len_utf16();
    }
    text.chars().count()
}

// ---- commands and code actions -------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Command {
    pub title: String,
    pub command: String,
    pub arguments: Vec<Value>,
}

pub fn command(value: &Value) -> Option<Command> {
    Some(Command {
        title: text(value, "title").unwrap_or_default(),
        command: text(value, "command")?,
        arguments: value
            .get("arguments")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default(),
    })
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeAction {
    pub title: String,
    pub kind: Option<String>,
    pub preferred: bool,
    /// Why the action cannot be used right now.
    pub disabled: Option<String>,
    pub edit: Option<WorkspaceEdit>,
    pub command: Option<Command>,
    /// Both `edit` and `command` are missing: ask the server to fill them in.
    pub needs_resolve: bool,
    /// The action exactly as sent, for `codeAction/resolve`.
    pub raw: Value,
}

/// `(Command | CodeAction)[] | null`.
pub fn code_actions(value: &Value, encoding: PositionEncoding) -> Vec<CodeAction> {
    items(value)
        .filter_map(|item| {
            let title = text(item, "title")?;
            // A bare Command has `command` as a string and nothing else of note.
            if item.get("command").is_some_and(Value::is_string) {
                return Some(CodeAction {
                    title,
                    kind: None,
                    preferred: false,
                    disabled: None,
                    edit: None,
                    command: command(item),
                    needs_resolve: false,
                    raw: item.clone(),
                });
            }
            let edit = item
                .get("edit")
                .and_then(|e| workspace_edit(e, encoding).ok());
            let command = item.get("command").and_then(command);
            Some(CodeAction {
                title,
                kind: text(item, "kind"),
                preferred: item.get("isPreferred").and_then(Value::as_bool) == Some(true),
                disabled: item
                    .get("disabled")
                    .map(|d| text(d, "reason").unwrap_or_default()),
                needs_resolve: edit.is_none() && command.is_none(),
                edit,
                command,
                raw: item.clone(),
            })
        })
        .collect()
}

// ---- rename --------------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenameTarget {
    /// `None` when the server only says the position can be renamed.
    pub span: Option<Span>,
    pub placeholder: Option<String>,
}

/// `Range | { range, placeholder } | { defaultBehavior: true } | null`.
pub fn prepare_rename(value: &Value, encoding: PositionEncoding) -> Option<RenameTarget> {
    if value.is_null() {
        return None;
    }
    if value.get("defaultBehavior").and_then(Value::as_bool) == Some(true) {
        return Some(RenameTarget {
            span: None,
            placeholder: None,
        });
    }
    if let Some(range) = value.get("range") {
        return Some(RenameTarget {
            span: Some(span(range, encoding)?),
            placeholder: text(value, "placeholder"),
        });
    }
    Some(RenameTarget {
        span: Some(span(value, encoding)?),
        placeholder: None,
    })
}

// ---- hints, folding, selection, lenses -----------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InlayHint {
    pub position: Position,
    pub encoding: PositionEncoding,
    pub label: String,
    /// 1 for a type, 2 for a parameter name.
    pub kind: Option<u32>,
    pub padding_left: bool,
    pub padding_right: bool,
}

pub fn inlay_hints(value: &Value, encoding: PositionEncoding) -> Vec<InlayHint> {
    items(value)
        .filter_map(|item| {
            let label = match item.get("label")? {
                Value::String(label) => label.clone(),
                Value::Array(parts) => parts
                    .iter()
                    .filter_map(|p| p.get("value").and_then(Value::as_str))
                    .collect(),
                _ => return None,
            };
            Some(InlayHint {
                position: position(item.get("position")?)?,
                encoding,
                label,
                kind: item
                    .get("kind")
                    .and_then(Value::as_u64)
                    .and_then(|k| u32::try_from(k).ok()),
                padding_left: item.get("paddingLeft").and_then(Value::as_bool) == Some(true),
                padding_right: item.get("paddingRight").and_then(Value::as_bool) == Some(true),
            })
        })
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FoldingRange {
    pub start_line: u32,
    pub end_line: u32,
    pub kind: Option<String>,
}

pub fn folding_ranges(value: &Value) -> Vec<FoldingRange> {
    items(value)
        .filter_map(|item| {
            let start = u32::try_from(item.get("startLine")?.as_u64()?).ok()?;
            let end = u32::try_from(item.get("endLine")?.as_u64()?).ok()?;
            (end > start).then(|| FoldingRange {
                start_line: start,
                end_line: end,
                kind: text(item, "kind"),
            })
        })
        .collect()
}

/// The chain of ranges around a position, innermost first.
pub fn selection_chain(value: &Value, encoding: PositionEncoding) -> Vec<Span> {
    let mut chain = Vec::new();
    let mut at = items(value).next();
    while let Some(node) = at {
        let Some(range) = node.get("range").and_then(|r| span(r, encoding)) else {
            break;
        };
        chain.push(range);
        at = node.get("parent").filter(|p| p.is_object());
        if chain.len() > 64 {
            break;
        }
    }
    chain
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodeLens {
    pub span: Span,
    pub command: Option<Command>,
    pub raw: Value,
}

pub fn code_lenses(value: &Value, encoding: PositionEncoding) -> Vec<CodeLens> {
    items(value)
        .filter_map(|item| {
            Some(CodeLens {
                span: span(item.get("range")?, encoding)?,
                command: item.get("command").and_then(command),
                raw: item.clone(),
            })
        })
        .collect()
}

// ---- hierarchies ---------------------------------------------------------------

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HierarchyItem {
    pub name: String,
    pub detail: Option<String>,
    pub kind: u32,
    pub uri: String,
    pub full: Span,
    pub selection: Span,
    /// The item exactly as sent, for the follow-up request.
    pub raw: Value,
}

fn hierarchy_item(item: &Value, encoding: PositionEncoding) -> Option<HierarchyItem> {
    Some(HierarchyItem {
        name: text(item, "name")?,
        detail: text(item, "detail"),
        kind: u32::try_from(item.get("kind")?.as_u64()?).ok()?,
        uri: text(item, "uri")?,
        full: span(item.get("range")?, encoding)?,
        selection: span(item.get("selectionRange")?, encoding)?,
        raw: item.clone(),
    })
}

/// The result of `prepareCallHierarchy` or `prepareTypeHierarchy`, and of the
/// type hierarchy's `supertypes` and `subtypes`.
pub fn hierarchy_items(value: &Value, encoding: PositionEncoding) -> Vec<HierarchyItem> {
    items(value)
        .filter_map(|item| hierarchy_item(item, encoding))
        .collect()
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Call {
    pub item: HierarchyItem,
    /// Where in the caller the call happens (incoming) or in the origin (outgoing).
    pub sites: Vec<Span>,
}

/// `incomingCalls` (the `from` side) or `outgoingCalls` (the `to` side).
pub fn calls(value: &Value, side: &str, encoding: PositionEncoding) -> Vec<Call> {
    items(value)
        .filter_map(|item| {
            Some(Call {
                item: hierarchy_item(item.get(side)?, encoding)?,
                sites: items(item.get("fromRanges").unwrap_or(&Value::Null))
                    .filter_map(|r| span(r, encoding))
                    .collect(),
            })
        })
        .collect()
}

#[cfg(test)]
#[path = "reply_tests.rs"]
mod tests;
