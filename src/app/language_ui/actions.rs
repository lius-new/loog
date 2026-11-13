//! The commands that ask a server something and then do something with the
//! answer: go to definition and its relatives, references, symbols.
use super::navigate::{Place, display_resource};
use super::popup::{Item, Kind, Payload, Popup};
use super::{AppContext, AppState, Target, services, target};
use crate::features::language_service::reply::{self, Location, Symbol};
use crate::features::language_service::request::Feature;
use crate::features::language_service::uri;
use crate::features::providers::{Failure, RequestHandle, RequestSpec};
use lgui::prelude::UiRect;
use serde_json::json;
use std::sync::Arc;

/// What the answer is wanted for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Wanted {
    Goto(Feature),
    Peek(Feature),
    References,
    DocumentSymbols,
    WorkspaceSymbols,
}

/// A request in flight whose answer the person is waiting for.
#[derive(Clone)]
pub struct Pending {
    pub wanted: Wanted,
    pub handle: Arc<RequestHandle>,
    pub target: Target,
    pub rect: UiRect,
}

/// Something that must happen outside the state update that found it out.
pub enum Effect {
    Go(Place, UiRect),
    Peek(Place),
}

/// Asks the servers, replacing any earlier question still unanswered.
pub fn begin(state: &AppContext, wanted: Wanted, rect: UiRect, query: Option<String>) {
    let app = state.get();
    let pane = app.workspace.active_pane();
    let Some(target) = target(&app, pane) else {
        return;
    };
    let Some(services) = services(state) else {
        return;
    };
    let (feature, extra) = match wanted {
        Wanted::Goto(feature) | Wanted::Peek(feature) => (feature, serde_json::Value::Null),
        Wanted::References => (
            Feature::References,
            json!({ "context": { "includeDeclaration": true } }),
        ),
        Wanted::DocumentSymbols => (Feature::DocumentSymbol, serde_json::Value::Null),
        Wanted::WorkspaceSymbols => (
            Feature::WorkspaceSymbol,
            json!({ "query": query.unwrap_or_default() }),
        ),
    };
    let handle = Arc::new(
        services.request(
            RequestSpec::new(
                feature,
                target.resource.clone(),
                target.lsp_stamp(),
                target.cursor,
            )
            .extra(extra),
        ),
    );
    let finished = handle.is_finished();
    state.update(|app| {
        app.editor.language.pending = Some(Pending {
            wanted,
            handle,
            target,
            rect,
        });
    });
    // Unsupported requests finish synchronously and have no provider event to wake the UI.
    if finished {
        super::tick(state);
    }
}

/// Where `parts` say to go, without repeats.
pub(super) fn locations(
    parts: &[Arc<crate::features::providers::Part>],
    feature: Feature,
) -> Vec<Place> {
    distinct(parts, feature)
        .iter()
        .filter_map(place_of)
        .collect()
}

fn place_of(location: &Location) -> Option<Place> {
    Some(Place::resource_span(
        location.resource.clone(),
        location.span,
    ))
}

/// Locations without repeats, in the order the servers gave them.
fn distinct(parts: &[Arc<crate::features::providers::Part>], feature: Feature) -> Vec<Location> {
    let mut seen = std::collections::HashSet::new();
    let mut all = Vec::new();
    for part in parts {
        let _ = feature;
        for location in reply::locations(&part.value, part.encoding) {
            let key = (
                location.resource.clone(),
                location.span.start,
                location.span.end,
            );
            if seen.insert(key) {
                all.push(location);
            }
        }
    }
    all
}

fn why_nothing(wanted: Wanted, failure: Option<&Failure>) -> String {
    let what = match wanted {
        Wanted::Goto(Feature::Declaration) => "declaration",
        Wanted::Goto(Feature::TypeDefinition) => "type definition",
        Wanted::Goto(Feature::Implementation) => "implementation",
        Wanted::Goto(_) | Wanted::Peek(_) => "definition",
        Wanted::References => "references",
        Wanted::DocumentSymbols | Wanted::WorkspaceSymbols => "symbols",
    };
    match failure {
        Some(Failure::Unsupported) => {
            format!("No language server here can find the {what}")
        }
        Some(Failure::Stale) => "The text changed; try again".to_owned(),
        Some(Failure::Busy) => "The language servers are busy".to_owned(),
        Some(Failure::Failed(reason)) => format!("The language server failed: {reason}"),
        None => format!("No {what} found"),
    }
}

/// Takes the answer of the pending request when it is complete.
pub fn tick(app: &mut AppState) -> Vec<Effect> {
    let Some(pending) = app.editor.language.pending.clone() else {
        return Vec::new();
    };
    let progress = pending.handle.poll();
    if !progress.finished {
        return Vec::new();
    }
    app.editor.language.pending = None;
    // Answers for a document the person has since left are dropped.
    let target = &pending.target;
    let same_document = app.workspace.active_pane() == target.pane
        && super::target(app, target.pane).is_some_and(|now| {
            now.same_view(target)
                && now.stamp == target.stamp
                && now.cursor == target.cursor
                && now.selection == target.selection
        });
    if !same_document && pending.wanted != Wanted::WorkspaceSymbols {
        return Vec::new();
    }
    let mut effects = Vec::new();
    match pending.wanted {
        Wanted::Goto(feature) | Wanted::Peek(feature) => {
            let mut found = distinct(&progress.parts, feature);
            match found.len() {
                0 => app.show_toast(why_nothing(pending.wanted, progress.failure.as_ref())),
                1 => {
                    if let Some(place) = place_of(&found.remove(0)) {
                        effects.push(if matches!(pending.wanted, Wanted::Peek(_)) {
                            Effect::Peek(place)
                        } else {
                            Effect::Go(place, pending.rect)
                        });
                    }
                }
                _ if same_document => {
                    let mut items = location_items(app, &found, false);
                    if matches!(pending.wanted, Wanted::Peek(_)) {
                        for item in &mut items {
                            if let Payload::Place(place) = &item.payload {
                                item.payload = Payload::PeekPlace(place.clone());
                            }
                        }
                    }
                    app.editor.language.popup = Some(
                        Popup::new(
                            Kind::Locations,
                            target.pane,
                            target.stamp,
                            Some(target.cursor),
                            items,
                        )
                        .titled(format!("{} results", found.len())),
                    );
                }
                _ => {}
            }
        }
        Wanted::References => {
            let found = distinct(&progress.parts, Feature::References);
            if found.is_empty() {
                app.show_toast(why_nothing(pending.wanted, progress.failure.as_ref()));
            } else if same_document {
                let mut items = location_items(app, &found, true);
                items.sort_by_key(|item| match &item.payload {
                    Payload::Place(place) => Some(place.resource.clone()),
                    _ => None,
                });
                let mut grouped = Vec::new();
                let mut previous = None;
                for item in items {
                    if let Payload::Place(place) = &item.payload
                        && previous.as_ref() != Some(&place.resource)
                    {
                        grouped.push(Item {
                            label: display_resource(app, &place.resource),
                            detail: String::new(),
                            badge: "▾",
                            faint: false,
                            payload: Payload::Group(place.resource.clone()),
                        });
                        previous = Some(place.resource.clone());
                    }
                    grouped.push(item);
                }
                let mut panel =
                    Popup::new(Kind::References, target.pane, target.stamp, None, grouped)
                        .with_field()
                        .titled(format!("{} references", found.len()));
                panel.resize(pending.rect.height());
                panel.select(1);
                app.editor.language.popup = Some(panel);
            }
        }
        Wanted::DocumentSymbols => {
            let mut items = Vec::new();
            for part in progress.usable(Feature::DocumentSymbol) {
                let symbols = reply::symbols(&part.value, part.encoding);
                // COMPAT(native-symbol-navigation): The existing symbol list
                // navigates native paths. Use resource locations after that
                // consumer migrates; no removal version is scheduled.
                if let Some(path) = target.resource.local_path() {
                    flatten(&symbols, 0, path, &mut items);
                }
            }
            if items.is_empty() {
                app.show_toast(why_nothing(pending.wanted, progress.failure.as_ref()));
            } else if same_document {
                app.editor.language.popup = Some(
                    Popup::new(
                        Kind::DocumentSymbols,
                        target.pane,
                        target.stamp,
                        None,
                        items,
                    )
                    .with_field()
                    .titled("Go to symbol in file"),
                );
            }
        }
        Wanted::WorkspaceSymbols => {
            let mut items = Vec::new();
            for part in &progress.parts {
                for symbol in reply::symbols(&part.value, part.encoding) {
                    if let Some(path) = target.resource.local_path()
                        && let Some(item) = symbol_item(&symbol, 0, path)
                    {
                        items.push(item);
                    }
                }
            }
            // The popup is already open: only its rows change.
            if let Some(popup) = app
                .editor
                .language
                .popup
                .as_mut()
                .filter(|p| p.kind == Kind::WorkspaceSymbols)
            {
                popup.set_items_keeping_order(items);
            }
        }
    }
    effects
}

/// One row for a symbol.
fn symbol_item(symbol: &Symbol, depth: usize, current: &std::path::Path) -> Option<Item> {
    let path = match &symbol.uri {
        Some(uri) => uri::file_path(uri)?,
        None => current.to_path_buf(),
    };
    Some(Item {
        label: format!("{}{}", "  ".repeat(depth), symbol.name),
        detail: symbol
            .container
            .clone()
            .or_else(|| symbol.detail.clone())
            .unwrap_or_default(),
        badge: symbol_badge(symbol.kind),
        payload: Payload::Place(Place::span(path, symbol.selection)),
        faint: symbol.deprecated,
    })
}

fn flatten(symbols: &[Symbol], depth: usize, current: &std::path::Path, out: &mut Vec<Item>) {
    for symbol in symbols {
        out.extend(symbol_item(symbol, depth, current));
        flatten(&symbol.children, depth + 1, current, out);
    }
}

/// A short name for a `SymbolKind`.
pub fn symbol_badge(kind: u32) -> &'static str {
    match kind {
        1 => "F",
        2..=4 => "{}",
        5 => "C",
        6 => "m",
        7 | 8 => "p",
        9 => "c",
        10 => "E",
        11 => "I",
        12 => "fn",
        13 => "v",
        14 => "k",
        15..=21 => "·",
        22 => "e",
        23 => "S",
        24 => "ev",
        25 => "op",
        26 => "T",
        _ => "",
    }
}

fn location_items(app: &AppState, found: &[Location], with_preview: bool) -> Vec<Item> {
    found
        .iter()
        .filter_map(|location| {
            let place = place_of(location)?;
            let name = display_resource(app, &place.resource);
            let text = app
                .workspace
                .file_id_for_resource(&place.resource)
                .and_then(|id| app.workspace.documents.get(&id))
                .map(|doc| doc.buffer.text_snapshot());
            let line = location.span.start.line as usize;
            let preview = text
                .as_ref()
                .and_then(|text| text.lines().nth(line))
                .map(|l| l.trim().to_owned())
                .unwrap_or_default();
            Some(Item {
                label: format!("{name}:{}", line + 1),
                detail: if with_preview || !preview.is_empty() {
                    preview
                } else {
                    String::new()
                },
                badge: "",
                payload: Payload::Place(place),
                faint: false,
            })
        })
        .collect()
}

/// The diagnostic after (or, going backwards, before) `cursor`, wrapping around.
pub fn next_diagnostic(
    diagnostics: &[crate::features::language_service::diagnostics::Diagnostic],
    cursor: usize,
    forward: bool,
) -> Option<usize> {
    let mut starts: Vec<usize> = diagnostics.iter().map(|d| d.range.start).collect();
    starts.sort_unstable();
    starts.dedup();
    if forward {
        starts
            .iter()
            .copied()
            .find(|start| *start > cursor)
            .or_else(|| starts.first().copied())
    } else {
        starts
            .iter()
            .copied()
            .rev()
            .find(|start| *start < cursor)
            .or_else(|| starts.last().copied())
    }
}

/// Moves to the next or previous problem the servers reported for the open file.
pub fn go_to_diagnostic(state: &AppContext, forward: bool, rect: UiRect) {
    let app = state.get();
    let pane = app.workspace.active_pane();
    let Some(target) = target(&app, pane) else {
        return;
    };
    let Some(services) = services(state) else {
        return;
    };
    let found = services
        .results(&target.resource, &target.stamp)
        .diagnostics
        .unwrap_or_else(|| Arc::from(Vec::new()));
    let Some(start) = next_diagnostic(&found, target.cursor, forward) else {
        state.update(|app| app.show_toast("No problems in this file"));
        return;
    };
    state.update(|app| {
        if let Some(mut editor) = app.workspace.active_editor_mut() {
            editor.set_cursor(start);
        }
        crate::app::editor::reveal_cursor(app, rect);
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::language_service::diagnostics::{Diagnostic, Severity};

    fn at(start: usize) -> Diagnostic {
        Diagnostic {
            documentation_url: None,
            range: start..start + 1,
            severity: Severity::Error,
            message: String::new(),
            source: None,
            code: None,
        }
    }

    #[test]
    fn the_next_diagnostic_is_after_the_cursor_and_wraps() {
        let found = [at(30), at(10), at(20)];
        assert_eq!(next_diagnostic(&found, 10, true), Some(20));
        assert_eq!(next_diagnostic(&found, 25, true), Some(30));
        assert_eq!(
            next_diagnostic(&found, 30, true),
            Some(10),
            "wraps to the first"
        );
        assert_eq!(next_diagnostic(&found, 0, true), Some(10));
    }

    #[test]
    fn the_previous_diagnostic_is_before_the_cursor_and_wraps() {
        let found = [at(30), at(10), at(20)];
        assert_eq!(next_diagnostic(&found, 25, false), Some(20));
        assert_eq!(
            next_diagnostic(&found, 10, false),
            Some(30),
            "wraps to the last"
        );
        assert_eq!(next_diagnostic(&[], 0, true), None);
    }

    #[test]
    fn symbol_kinds_get_short_badges() {
        assert_eq!(symbol_badge(12), "fn");
        assert_eq!(symbol_badge(23), "S");
        assert_eq!(symbol_badge(999), "");
    }
}
