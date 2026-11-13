//! Typed provider requests and answers at the editor boundary.
use super::*;
use crate::features::language_service::{
    capabilities::PositionEncoding, position::LineIndex, request::Feature, uri,
};
use loog_extension_host::{
    ColumnUnit, FileRange, ProviderContext, ProviderMode, WireLanguageAnswer as Answer,
};
use serde_json::{Value, json};
pub(super) fn supports_mode(
    feature: Feature,
    config: Option<&loog_extension_host::ProviderConfiguration>,
) -> bool {
    let mode = match feature {
        Feature::Definition => ProviderMode::Definition,
        Feature::Declaration => ProviderMode::Declaration,
        Feature::TypeDefinition => ProviderMode::TypeDefinition,
        Feature::Implementation => ProviderMode::Implementation,
        Feature::Formatting => ProviderMode::Full,
        Feature::RangeFormatting => ProviderMode::Range,
        _ => return true,
    };
    config
        .and_then(|configuration| configuration.modes.as_ref())
        .is_none_or(|modes| modes.contains(&mode))
}
pub(super) fn same_configuration(
    a: &loog_extension_host::ProviderConfiguration,
    b: &loog_extension_host::ProviderConfiguration,
) -> bool {
    a.provider == b.provider
        && a.enabled == b.enabled
        && a.modes == b.modes
        && a.revision == b.revision
        && a.resolve == b.resolve
        && a.prepare == b.prepare
        && a.triggers == b.triggers
        && a.retriggers == b.retriggers
        && a.commands == b.commands
        && a.aliases == b.aliases
}

#[derive(Debug)]
pub enum WorkspaceAction {
    Edit {
        title: String,
        edit: crate::features::language_service::reply::WorkspaceEdit,
        stamps: Vec<(loog_workspace_api::ResourceRef, DocumentStamp)>,
    },
    Show {
        path: PathBuf,
        span: crate::features::language_service::reply::Span,
    },
}

impl ExtensionService {
    pub(super) fn workspace_operation(
        &self,
        app: &AppState,
        id: &str,
        generation: u64,
        operation: u64,
        request: loog_extension_host::WorkspaceOperation,
    ) -> Vec<Effect> {
        let result = (|| {
            let package = self
                .packages
                .get(id)
                .filter(|p| p.lifecycle.accepts(generation))
                .ok_or_else(|| {
                    ExtensionError::new(
                        ErrorKind::ExpiredHandle,
                        "the extension is no longer active",
                    )
                })?;
            self.authorize(package, "documents.read")?;
            Ok(match request {
                loog_extension_host::WorkspaceOperation::Edit(request) => {
                    self.authorize(package, "workspace.edit")?;
                    self.validate_workspace_edit(app, package, &request.edit)?;
                    let value = workspace_edit(request.edit).ok_or_else(|| {
                        ExtensionError::new(ErrorKind::InvalidEdit, "invalid workspace edit")
                    })?;
                    let edit = crate::features::language_service::reply::workspace_edit(
                        &value,
                        PositionEncoding::Utf8,
                    )
                    .map_err(|s| ExtensionError::new(ErrorKind::InvalidEdit, s))?;
                    WorkspaceAction::Edit {
                        title: request.title,
                        edit,
                        stamps: crate::app::language_ui::edit::stamps(app),
                    }
                }
                loog_extension_host::WorkspaceOperation::ShowDocument(location) => {
                    if !Path::new(&location.path).is_absolute() {
                        return Err(ExtensionError::new(
                            ErrorKind::InvalidEdit,
                            "navigation needs an absolute file path",
                        ));
                    }
                    let span = range(location.range)
                        .and_then(|value| {
                            crate::features::language_service::reply::span(
                                &value,
                                PositionEncoding::Utf8,
                            )
                        })
                        .ok_or_else(|| {
                            ExtensionError::new(ErrorKind::InvalidEdit, "invalid navigation range")
                        })?;
                    WorkspaceAction::Show {
                        path: PathBuf::from(location.path),
                        span,
                    }
                }
            })
        })();
        match result {
            Ok(action) => vec![Effect::WorkspaceOperation {
                id: id.into(),
                generation,
                operation,
                action,
            }],
            Err(error) => vec![Effect::Submit(Job::Complete {
                id: id.into(),
                generation,
                operation,
                outcome: Outcome::Failed(error),
            })],
        }
    }
    fn validate_workspace_edit(
        &self,
        app: &AppState,
        package: &Package,
        edit: &loog_extension_host::WireWorkspaceEdit,
    ) -> Result<(), ExtensionError> {
        let check = |value: &str, resource: bool| {
            let path = Path::new(value);
            if !path.is_absolute() || value.contains('\0') {
                return Err(ExtensionError::new(
                    ErrorKind::InvalidEdit,
                    "workspace edits need absolute file paths",
                ));
            }
            if !resource
                && app
                    .workspace
                    .documents
                    .values()
                    .any(|d| d.meta.local_path() == Some(path))
            {
                return Ok(());
            }
            if resource {
                self.authorize(package, "workspace.files.write")?;
            }
            for root in app.explorer.model.roots() {
                if let Ok(suffix) = path.strip_prefix(root) {
                    let relative = suffix.to_string_lossy().replace('\\', "/");
                    files::within(root, &relative)?;
                    if !resource && let Ok(real) = path.canonicalize() {
                        let base = root.canonicalize().map_err(|_| {
                            ExtensionError::new(
                                ErrorKind::ExpiredHandle,
                                "workspace root is unavailable",
                            )
                        })?;
                        if !real.starts_with(base) {
                            return Err(ExtensionError::new(
                                ErrorKind::PermissionDenied,
                                "a text edit follows a link outside the workspace",
                            ));
                        }
                    }
                    return Ok(());
                }
            }
            Err(ExtensionError::new(
                ErrorKind::PermissionDenied,
                "the edit is outside open documents and workspace folders",
            ))
        };
        for change in &edit.changes {
            match change {
                loog_extension_host::WorkspaceChange::Text(d) => check(&d.path, false)?,
                loog_extension_host::WorkspaceChange::Create(op) => check(&op.path, true)?,
                loog_extension_host::WorkspaceChange::Rename(op) => {
                    check(&op.source, true)?;
                    check(&op.target, true)?;
                }
                loog_extension_host::WorkspaceChange::Delete(op) => check(&op.path, true)?,
            }
        }
        Ok(())
    }
    pub(super) fn validate_language_answer(
        &self,
        app: &AppState,
        package: &Package,
        answer: &Answer,
    ) -> Result<(), ExtensionError> {
        match answer {
            Answer::WorkspaceEdit(edit) => {
                self.authorize(package, "workspace.edit")?;
                self.validate_workspace_edit(app, package, edit)
            }
            Answer::Actions(actions) => {
                for a in actions {
                    if let Some(edit) = &a.edit {
                        self.authorize(package, "workspace.edit")?;
                        self.validate_workspace_edit(app, package, edit)?;
                    }
                }
                Ok(())
            }
            _ => Ok(()),
        }
    }
    pub(super) fn cancel_provider(&mut self, request: u64, effects: &mut Vec<Effect>) {
        let Some(call) = self.provider_calls.remove(&request) else {
            return;
        };
        self.queued.retain(|queued| *queued != request);
        effects.push(Effect::Submit(Job::CancelProvider {
            id: call.extension.clone(),
            generation: call.generation,
            request,
        }));
        if let Some(token) = self.language_calls.remove(&request) {
            effects.push(Effect::LanguageAnswer {
                token,
                answer: None,
            });
        } else if matches!(call.job.kind, WireKind::Completion) {
            effects.push(Effect::OfferCompletion {
                file: call.file,
                incarnation: call.stamp.incarnation,
                word_start: call.word_start,
                source: self.source_for(&call.extension, &call.provider),
                rank: COMPLETION_RANK,
                items: None,
            });
        }
        if let Some(chain) = self.save_calls.remove(&request) {
            self.fail(
                chain,
                if (self.clock)() >= call.deadline {
                    "save participant answered too late".into()
                } else {
                    "save participant stopped or timed out".into()
                },
                effects,
            );
        }
    }
    pub(super) fn capability_sources(
        &mut self,
        resource: Option<&loog_workspace_api::ResourceRef>,
    ) -> Vec<crate::app::language_services::CapabilitySource> {
        use crate::features::language_service::providers::{
            CodeActionOptions, CompletionOptions, OnTypeFormatting, Providers, SignatureOptions,
        };
        let registry = crate::domain::language::Registry::builtin();
        let provided = resource.is_some_and(|r| r.environment().is_some());
        let language = resource.and_then(|resource| {
            if let Some(path) = resource.local_path() {
                Some(
                    self.document_languages
                        .get(path)
                        .cloned()
                        .unwrap_or_else(|| {
                            registry.resolve_path(path).definition().name.to_string()
                        }),
                )
            } else {
                self.resource_documents
                    .language(resource)
                    .map(str::to_owned)
            }
        });
        let mut declarations = Vec::new();
        for (owner, package) in &self.packages {
            if !package.manifest.has_behaviour()
                || matches!(
                    package.lifecycle.state(),
                    State::Stopped | State::Failed | State::Stopping
                )
                || ["documents.read", "languages"]
                    .iter()
                    .any(|c| self.authorize(package, c).is_err())
            {
                continue;
            }
            for provider in &package.manifest.contributes.language_service.providers {
                if provided && !self.resource_provider_bound(owner, &provider.id) {
                    continue;
                }
                if language
                    .as_ref()
                    .is_some_and(|lang| !provider.selector.languages.contains(lang))
                {
                    continue;
                }
                let config = self.provider_configuration(package, &provider.id, resource);
                if config.is_some_and(|c| !c.enabled) {
                    continue;
                }
                let mut capabilities = Providers::default();
                let resolve = config.is_none_or(|c| c.resolve);
                let triggers = config.map(|c| c.triggers.clone()).unwrap_or_default();
                let mode = |mode| {
                    config
                        .and_then(|c| c.modes.as_ref())
                        .is_none_or(|modes| modes.contains(&mode))
                };
                match provider.kind {
                    DeclKind::Completion => {
                        capabilities.completion = Some(CompletionOptions {
                            resolve,
                            trigger_characters: triggers,
                        })
                    }
                    DeclKind::Diagnostics => capabilities.pull_diagnostics = true,
                    DeclKind::Formatting => {
                        capabilities.formatting = mode(ProviderMode::Full);
                        capabilities.range_formatting = mode(ProviderMode::Range);
                    }
                    DeclKind::Hover => capabilities.hover = true,
                    DeclKind::Definition => {
                        capabilities.definition = mode(ProviderMode::Definition);
                        capabilities.declaration = mode(ProviderMode::Declaration);
                        capabilities.type_definition = mode(ProviderMode::TypeDefinition);
                        capabilities.implementation = mode(ProviderMode::Implementation);
                    }
                    DeclKind::DocumentSymbols => capabilities.document_symbol = true,
                    DeclKind::SignatureHelp => {
                        capabilities.signature_help = Some(SignatureOptions {
                            trigger_characters: triggers,
                            retrigger_characters: config
                                .map(|c| c.retriggers.clone())
                                .unwrap_or_default(),
                        })
                    }
                    DeclKind::InlayHints => capabilities.inlay_hint = true,
                    DeclKind::CodeActions => {
                        capabilities.code_action = Some(CodeActionOptions { resolve })
                    }
                    DeclKind::References => capabilities.references = true,
                    DeclKind::WorkspaceSymbols => capabilities.workspace_symbol = true,
                    DeclKind::DocumentHighlights => capabilities.document_highlight = true,
                    DeclKind::Rename => {
                        capabilities.rename = Some(config.is_none_or(|c| c.prepare))
                    }
                    DeclKind::FoldingRanges => capabilities.folding_range = true,
                    DeclKind::SelectionRanges => capabilities.selection_range = true,
                    DeclKind::CallHierarchy => capabilities.call_hierarchy = true,
                    DeclKind::TypeHierarchy => capabilities.type_hierarchy = true,
                    DeclKind::CodeLens => capabilities.code_lens = true,
                    DeclKind::FormatOnType => {
                        if let Some(first) = triggers.first() {
                            capabilities.on_type_formatting = Some(OnTypeFormatting {
                                first: first.clone(),
                                more: triggers[1..].to_vec(),
                            });
                        }
                    }
                    DeclKind::ExecuteCommand => {
                        capabilities.execute_command =
                            config.map(|c| c.commands.clone()).unwrap_or_default()
                    }
                    DeclKind::SaveParticipant | DeclKind::Highlights => {}
                }
                declarations.push((
                    owner.clone(),
                    provider.id.clone(),
                    package.lifecycle.generation(),
                    package.provider_revision,
                    capabilities,
                ));
            }
        }
        declarations
            .into_iter()
            .map(|(owner, provider, run, revision, providers)| {
                crate::app::language_services::CapabilitySource {
                    id: self.source_for(&owner, &provider),
                    run,
                    revision,
                    providers,
                }
            })
            .collect()
    }
}

pub(super) fn context(ask: &LanguageAsk, text: &str) -> ProviderContext {
    let mode = match ask.feature {
        Feature::Definition => Some(ProviderMode::Definition),
        Feature::Declaration => Some(ProviderMode::Declaration),
        Feature::TypeDefinition => Some(ProviderMode::TypeDefinition),
        Feature::Implementation => Some(ProviderMode::Implementation),
        Feature::PrepareCallHierarchy | Feature::PrepareTypeHierarchy | Feature::PrepareRename => {
            Some(ProviderMode::Prepare)
        }
        Feature::IncomingCalls => Some(ProviderMode::Incoming),
        Feature::OutgoingCalls => Some(ProviderMode::Outgoing),
        Feature::Supertypes => Some(ProviderMode::Supertypes),
        Feature::Subtypes => Some(ProviderMode::Subtypes),
        Feature::RangeFormatting => Some(ProviderMode::Range),
        Feature::CodeActionResolve | Feature::CodeLensResolve => Some(ProviderMode::Resolve),
        _ => None,
    };
    let extra = &ask.extra;
    let data = if ask.feature == Feature::ExecuteCommand {
        extra.get("arguments")
    } else {
        extra.get("data")
    };
    ProviderContext {
        mode,
        name: extra
            .get("newName")
            .or_else(|| extra.get("command"))
            .and_then(Value::as_str)
            .map(str::to_owned),
        query: if ask.feature == Feature::Completion {
            let start = crate::features::language_service::completion::word_start(text, ask.start);
            text.get(start..ask.start)
                .filter(|query| {
                    query.len() <= 256
                        && extra
                            .pointer("/context/triggerKind")
                            .and_then(Value::as_u64)
                            != Some(2)
                })
                .map(str::to_owned)
        } else {
            extra
                .get("query")
                .and_then(Value::as_str)
                .map(str::to_owned)
        },
        include_declaration: extra
            .pointer("/context/includeDeclaration")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        tab_size: extra
            .pointer("/options/tabSize")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok())
            .unwrap_or(4),
        insert_spaces: extra
            .pointer("/options/insertSpaces")
            .and_then(Value::as_bool)
            .unwrap_or(true),
        action_kinds: extra
            .pointer("/context/only")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        trigger_kind: extra
            .pointer("/context/triggerKind")
            .and_then(Value::as_u64)
            .and_then(|n| u8::try_from(n).ok())
            .unwrap_or(1),
        retrigger: extra
            .pointer("/context/isRetrigger")
            .and_then(Value::as_bool)
            .unwrap_or(false),
        active_signature: extra
            .pointer("/context/activeSignatureHelp/activeSignature")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
        active_parameter: extra
            .pointer("/context/activeSignatureHelp/activeParameter")
            .and_then(Value::as_u64)
            .and_then(|n| u32::try_from(n).ok()),
        data: data.map(|v| {
            v.as_str()
                .map(str::to_owned)
                .unwrap_or_else(|| v.to_string())
        }),
    }
}

pub(super) fn completions(
    owner: &str,
    provider: &str,
    text: &str,
    index: &crate::features::providers::position::LineIndex<'_>,
    items: Vec<loog_extension_host::WireCompletionItem>,
) -> (Vec<Value>, usize) {
    let offered = items
        .into_iter()
        .map(|i| providers::Offered {
            label: i.label,
            label_detail: i.label_detail,
            label_description: i.label_description,
            deprecated: i.deprecated,
            preselect: i.preselect,
            insert_text_mode: i.insert_text_mode,
            detail: i.detail,
            documentation: i.documentation,
            kind: i.kind,
            insert_text: i.insert_text,
            text_edit: i
                .text_edit
                .map(|e| (e.range.start as usize..e.range.end as usize, e.text)),
            additional_text_edits: i
                .additional_text_edits
                .into_iter()
                .map(|e| (e.range.start as usize..e.range.end as usize, e.text))
                .collect(),
            insert_text_format: i.insert_text_format,
            commit_characters: i.commit_characters,
            command: i.command.map(|c| (c.name, c.arguments)),
            sort_text: i.sort_text,
            filter_text: i.filter_text,
            data: i.data,
        })
        .collect::<Vec<_>>();
    providers::completion_json_indexed(owner, provider, text, &offered, index)
}
pub(super) fn completion_item(raw: &Value) -> Option<loog_extension_host::WireCompletionItem> {
    let text = |key: &str| raw.get(key).and_then(Value::as_str).map(str::to_owned);
    let edit = |value: &Value| {
        Some(loog_extension_host::TextEdit {
            range: loog_extension_host::ByteRange {
                start: value["start"].as_u64()?,
                end: value["end"].as_u64()?,
            },
            text: value["text"].as_str()?.into(),
        })
    };
    Some(loog_extension_host::WireCompletionItem {
        label: text("label")?,
        label_detail: raw
            .pointer("/labelDetails/detail")
            .and_then(Value::as_str)
            .map(str::to_owned),
        label_description: raw
            .pointer("/labelDetails/description")
            .and_then(Value::as_str)
            .map(str::to_owned),
        deprecated: raw["deprecated"].as_bool(),
        preselect: raw["preselect"].as_bool(),
        insert_text_mode: raw["insertTextMode"]
            .as_u64()
            .and_then(|value| u8::try_from(value).ok()),
        detail: text("detail"),
        documentation: text("documentation"),
        kind: raw["kind"].as_u64().and_then(|v| u32::try_from(v).ok()),
        insert_text: text("insertText"),
        text_edit: raw["x-loog"].get("text-edit").and_then(edit),
        additional_text_edits: raw["x-loog"]["additional-text-edits"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(edit)
            .collect(),
        insert_text_format: raw["insertTextFormat"]
            .as_u64()
            .and_then(|v| u8::try_from(v).ok()),
        commit_characters: raw["commitCharacters"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(Value::as_str)
            .map(str::to_owned)
            .collect(),
        command: raw.get("command").and_then(|c| {
            Some(loog_extension_host::ProviderCommand {
                name: c["command"].as_str()?.into(),
                arguments: c["arguments"].to_string(),
            })
        }),
        sort_text: text("sortText"),
        filter_text: text("filterText"),
        data: raw["x-loog"]["data"].as_str().map(str::to_owned),
    })
}

pub(super) fn fits(kind: &WireKind, answer: &Provided) -> bool {
    match answer {
        Provided::Edits { .. } => matches!(kind, WireKind::Formatting | WireKind::FormatOnType),
        Provided::Language { answer, .. } => matches!(
            (kind, answer),
            (
                WireKind::Definition | WireKind::References,
                Answer::Places(_)
            ) | (WireKind::WorkspaceSymbols, Answer::WorkspaceSymbols(_))
                | (WireKind::DocumentHighlights, Answer::DocumentHighlights(_))
                | (WireKind::PrepareRename, Answer::RenameTarget(_))
                | (WireKind::Rename, Answer::WorkspaceEdit(_))
                | (WireKind::FoldingRanges, Answer::Folds(_))
                | (WireKind::SelectionRanges, Answer::Selections(_))
                | (
                    WireKind::CallHierarchy | WireKind::TypeHierarchy,
                    Answer::Hierarchy(_)
                )
                | (WireKind::CallHierarchy, Answer::Calls(_))
                | (
                    WireKind::CodeLens | WireKind::CodeLensResolve,
                    Answer::Lenses(_)
                )
                | (WireKind::Highlights, Answer::Highlights(_))
                | (WireKind::ExecuteCommand, Answer::Completed)
                | (WireKind::SignatureHelp, Answer::Signatures(_))
                | (
                    WireKind::CodeActions | WireKind::CodeActionResolve,
                    Answer::Actions(_)
                )
        ),
        Provided::CodeActions { .. } => matches!(kind, WireKind::CodeActions),
        _ => false,
    }
}

fn bytes(text: &str, range: loog_extension_host::ByteRange) -> Option<Value> {
    let start = usize::try_from(range.start).ok()?;
    let end = usize::try_from(range.end).ok()?;
    if start > end
        || end > text.len()
        || !text.is_char_boundary(start)
        || !text.is_char_boundary(end)
    {
        return None;
    }
    let index = LineIndex::new(text);
    let start = index.position(start, PositionEncoding::Utf8);
    let end = index.position(end, PositionEncoding::Utf8);
    Some(
        json!({"start":{"line":start.line,"character":start.character},"end":{"line":end.line,"character":end.character}}),
    )
}
fn range(range: FileRange) -> Option<Value> {
    if (range.start.line, range.start.column) > (range.end.line, range.end.column) {
        return None;
    }
    Some(
        json!({"start":{"line":range.start.line,"character":range.start.column},"end":{"line":range.end.line,"character":range.end.column},"x-loog-column-unit":match range.unit { ColumnUnit::Utf8 => "utf8", ColumnUnit::Utf16 => "utf16" }}),
    )
}
fn path(path: &str) -> Option<String> {
    let path = Path::new(path);
    if !path.is_absolute() || path.as_os_str().is_empty() {
        return None;
    }
    uri::file_uri(path)
}
fn place(place: loog_extension_host::FileLocation) -> Option<Value> {
    Some(json!({"uri":path(&place.path)?, "range":range(place.range)?}))
}
fn hierarchy(item: loog_extension_host::WireHierarchyItem) -> Option<Value> {
    Some(
        json!({"name":item.name,"kind":item.kind,"detail":item.detail,"uri":path(&item.location.path)?,"range":range(item.location.range)?,"selectionRange":range(item.selection)?,"data":item.data}),
    )
}
pub(super) fn edits(text: &str, edits: Vec<loog_extension_host::TextEdit>) -> Option<Value> {
    Some(Value::Array(
        edits
            .into_iter()
            .map(|edit| Some(json!({"range":bytes(text, edit.range)?,"newText":edit.text})))
            .collect::<Option<Vec<_>>>()?,
    ))
}
pub(super) fn workspace_edit(edit: loog_extension_host::WireWorkspaceEdit) -> Option<Value> {
    let mut changes = Vec::new();
    // Resource paths are absolute here; the application's existing edit admission
    // checks open roots, versions, dirty files and transaction rollback.
    for op in edit.changes {
        let next = match op {
            loog_extension_host::WorkspaceChange::Text(document) => {
                let edits = document
                    .edits
                    .into_iter()
                    .map(|edit| Some(json!({"range":range(edit.range)?,"newText":edit.text})))
                    .collect::<Option<Vec<_>>>()?;
                json!({"textDocument":{"uri":path(&document.path)?,"version":document.version},"edits":edits})
            }
            loog_extension_host::WorkspaceChange::Create(op) => {
                let uri = path(&op.path)?;
                json!({"kind":"create","uri":uri,"options":{"overwrite":op.overwrite,"ignoreIfExists":op.ignore_if_exists}})
            }
            loog_extension_host::WorkspaceChange::Rename(op) => {
                json!({"kind":"rename","oldUri":path(&op.source)?,"newUri":path(&op.target)?,"options":{"overwrite":op.overwrite,"ignoreIfExists":op.ignore_if_exists}})
            }
            loog_extension_host::WorkspaceChange::Delete(op) => {
                json!({"kind":"delete","uri":path(&op.path)?,"options":{"recursive":op.recursive,"ignoreIfNotExists":op.ignore_if_not_exists}})
            }
        };
        changes.push(next);
    }
    Some(json!({"documentChanges":changes}))
}
pub(super) fn answer(
    text: &str,
    answer: Answer,
    context: Option<&ProviderContext>,
) -> Option<Value> {
    Some(match answer {
        Answer::Places(items) => Value::Array(items.into_iter().map(place).collect::<Option<Vec<_>>>()?),
        Answer::WorkspaceSymbols(items) => Value::Array(items.into_iter().map(|s| Some(json!({"name":s.name,"kind":s.kind,"containerName":s.container,"location":place(s.location)?}))).collect::<Option<Vec<_>>>()?),
        Answer::DocumentHighlights(items) => Value::Array(items.into_iter().map(|h| Some(json!({"range":bytes(text,h.range)?,"kind":h.kind}))).collect::<Option<Vec<_>>>()?),
        Answer::RenameTarget(target) => match target { None => Value::Null, Some(t) if t.default_behavior => json!({"defaultBehavior":true}), Some(t) => json!({"range":bytes(text,t.range?)?,"placeholder":t.placeholder}) },
        Answer::WorkspaceEdit(edit) => workspace_edit(edit)?,
        Answer::Folds(items) => Value::Array(items.into_iter().map(|r| json!({"startLine":r.start,"endLine":r.end,"startCharacter":r.start_column,"endCharacter":r.end_column,"kind":r.kind,"collapsedText":r.label})).collect()),
        Answer::Selections(items) => {
            if items.len()>256 {return None;}
            let mut ranges: Vec<Value> = Vec::new();
            let mut bounds:Vec<(loog_extension_host::ByteRange,usize)>=Vec::new();
            for (i,r) in items.into_iter().enumerate() {
                let (parent,depth) = match r.parent {
                    Some(p) if (p as usize) < i => {
                        let (outer,depth)=bounds[p as usize];if depth>=64 || outer.start>r.range.start || outer.end<r.range.end {return None;}
                        (Some(ranges[p as usize].clone()),depth+1)
                    }
                    Some(_)=>return None,None=>(None,1)
                };
                bounds.push((r.range,depth));
                ranges.push(json!({"range":bytes(text,r.range)?,"parent":parent}));
            }
            // The requested selection is the last item; ancestors precede it.
            ranges.last().cloned().into_iter().collect::<Vec<_>>().into()
        }
        Answer::Hierarchy(items) => Value::Array(items.into_iter().map(hierarchy).collect::<Option<Vec<_>>>()?),
        Answer::Calls(items) => {
            let side = if context.and_then(|c|c.mode) == Some(ProviderMode::Incoming) { "from" } else { "to" };
            Value::Array(items.into_iter().map(|edge| {
                let sites = edge.sites.into_iter().map(range).collect::<Option<Vec<_>>>()?;
                let mut result = json!({"fromRanges":sites}); result[side] = hierarchy(edge.item)?; Some(result)
            }).collect::<Option<Vec<_>>>()?)
        }
        Answer::Lenses(items) => {
            let lenses = items.into_iter().map(|lens| Some(json!({"range":bytes(text,lens.range)?,"command":lens.command.map(|c| json!({"title":lens.title.unwrap_or_else(||c.name.clone()),"command":c.name,"arguments":serde_json::from_str::<Value>(&c.arguments).ok()})),"data":lens.data}))).collect::<Option<Vec<_>>>()?;
            if context.and_then(|c| c.mode) == Some(ProviderMode::Resolve) { lenses.into_iter().next().unwrap_or(Value::Null) } else { Value::Array(lenses) }
        }
        Answer::Highlights(_) => return None, // Published through the retained highlight channel.
        Answer::Actions(items) => {
            let actions = items.into_iter().map(|a| {
                let mut value = json!({"title":a.title,"kind":a.kind,"isPreferred":a.preferred,"data":a.data});
                if let Some(reason) = a.disabled { value["disabled"] = json!({"reason":reason}); }
                if let Some(edit) = a.edit { value["edit"] = workspace_edit(edit)?; }
                if let Some(c) = a.command { value["command"] = json!({"title":c.name,"command":c.name,"arguments":serde_json::from_str::<Value>(&c.arguments).ok()}); }
                Some(value)
            }).collect::<Option<Vec<_>>>()?;
            if context.and_then(|c|c.mode) == Some(ProviderMode::Resolve) { actions.into_iter().next().unwrap_or(Value::Null) } else { Value::Array(actions) }
        }
        Answer::Completed => Value::Null,
        Answer::Signatures(set) => {
            let signatures = set.signatures.into_iter().map(|signature| {
                let parameters = signature.parameters.into_iter().map(|p| {
                    let label = match p.range {
                        Some(range) => {
                            let start=usize::try_from(range.start).ok()?; let end=usize::try_from(range.end).ok()?;
                            if start > end || end > signature.label.len() || !signature.label.is_char_boundary(start) || !signature.label.is_char_boundary(end) { return None; }
                            json!([signature.label[..start].encode_utf16().count(),signature.label[..end].encode_utf16().count()])
                        }
                        None => json!(p.label),
                    }; Some(json!({"label":label,"documentation":p.documentation}))
                }).collect::<Option<Vec<_>>>()?;
                Some(json!({"label":signature.label,"documentation":signature.documentation,"parameters":parameters,"activeParameter":signature.active_parameter}))
            }).collect::<Option<Vec<_>>>()?;
            json!({"signatures":signatures,"activeSignature":set.active_signature,"activeParameter":set.active_parameter})
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::language_service::reply;
    #[test]
    fn completion_context_carries_the_bounded_native_filter_except_for_triggers() {
        let text = "fn 中变量a";
        let mut ask = LanguageAsk {
            feature: Feature::Completion,
            resource: loog_workspace_api::ResourceRef::local("test.rs"),
            stamp: crate::features::providers::Stamp {
                incarnation: 1,
                version: 0,
            },
            start: text.len(),
            end: text.len(),
            extra: Value::Null,
            target: None,
            server: None,
            timeout: None,
        };
        assert_eq!(context(&ask, text).query.as_deref(), Some("中变量a"));
        ask.extra = json!({"context":{"triggerKind":2}});
        assert!(context(&ask, text).query.is_none());
        ask.extra = Value::Null;
        ask.start = 257;
        assert!(context(&ask, &"a".repeat(257)).query.is_none());
        ask.start = 1;
        assert!(context(&ask, "中").query.is_none());
        ask.feature = Feature::WorkspaceSymbol;
        ask.extra = json!({"query":"search"});
        assert_eq!(context(&ask, "").query.as_deref(), Some("search"));
    }
    #[test]
    fn unopened_utf16_targets_keep_their_unit_through_native_navigation() {
        let span = range(FileRange {
            start: loog_extension_host::TextPosition { line: 0, column: 1 },
            end: loog_extension_host::TextPosition { line: 0, column: 3 },
            unit: ColumnUnit::Utf16,
        });
        let parsed = reply::span(&span.unwrap(), PositionEncoding::Utf8).unwrap();
        assert_eq!(parsed.bytes("a😀b"), 1..5);
    }
    #[test]
    fn multiple_signatures_preserve_the_active_overload_and_utf8_parameter_labels() {
        use loog_extension_host::{ByteRange, SignatureEntry, SignatureParameter, SignatureSet};
        let make = || SignatureEntry {
            label: "f(😀, 中)".into(),
            documentation: None,
            parameters: vec![SignatureParameter {
                label: "😀".into(),
                documentation: None,
                range: Some(ByteRange { start: 2, end: 6 }),
            }],
            active_parameter: Some(0),
        };
        let value = answer(
            "",
            Answer::Signatures(SignatureSet {
                signatures: vec![make(), make()],
                active_signature: 1,
                active_parameter: Some(0),
            }),
            None,
        )
        .unwrap();
        let help = reply::signature_help(&value).unwrap();
        assert_eq!(help.signatures.len(), 2);
        assert_eq!(help.active_signature, 1);
        assert_eq!(
            value["signatures"][1]["parameters"][0]["label"],
            json!([2, 4])
        );
    }
    #[test]
    fn selection_ancestors_must_contain_their_child_and_have_bounded_depth() {
        use loog_extension_host::{ByteRange, WireSelectionRange};
        assert!(
            answer(
                "abcdef",
                Answer::Selections(vec![
                    WireSelectionRange {
                        range: ByteRange { start: 0, end: 2 },
                        parent: None
                    },
                    WireSelectionRange {
                        range: ByteRange { start: 1, end: 5 },
                        parent: Some(0)
                    }
                ]),
                None
            )
            .is_none()
        );
        let chain = (0..65)
            .map(|i| WireSelectionRange {
                range: ByteRange { start: 0, end: 6 },
                parent: if i == 0 { None } else { Some(i - 1) },
            })
            .collect();
        assert!(answer("abcdef", Answer::Selections(chain), None).is_none());
    }
}
