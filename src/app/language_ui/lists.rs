//! Lists a server fills for the person to choose from: call and type hierarchies,
//! code lenses, and the problems of the open files.
use super::navigate::{Place, display_path};
use super::popup::{Item, Kind, Payload, Popup};
use super::{AppContext, AppState, Target, services, target};
use crate::features::language_service::diagnostics::Severity;
use crate::features::language_service::reply::{self, HierarchyItem};
use crate::features::language_service::request::Feature;
use crate::features::language_service::uri;
use crate::features::providers::{Failure, RequestHandle, RequestSpec};
use std::sync::Arc;

/// Which relation of a symbol to list.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Relation {
    Callers,
    Callees,
    Supertypes,
    Subtypes,
}

impl Relation {
    fn prepare(self) -> Feature {
        match self {
            Self::Callers | Self::Callees => Feature::PrepareCallHierarchy,
            Self::Supertypes | Self::Subtypes => Feature::PrepareTypeHierarchy,
        }
    }

    fn follow(self) -> Feature {
        match self {
            Self::Callers => Feature::IncomingCalls,
            Self::Callees => Feature::OutgoingCalls,
            Self::Supertypes => Feature::Supertypes,
            Self::Subtypes => Feature::Subtypes,
        }
    }

    fn title(self, of: &str) -> String {
        match self {
            Self::Callers => format!("Callers of {of}"),
            Self::Callees => format!("Calls made by {of}"),
            Self::Supertypes => format!("Supertypes of {of}"),
            Self::Subtypes => format!("Subtypes of {of}"),
        }
    }
}

#[derive(Clone)]
enum Step {
    /// Finding the symbol under the caret.
    Prepare,
    /// Asking what relates to it.
    Follow {
        instance: u64,
        item: Arc<HierarchyItem>,
    },
    Lenses,
    ResolveLens {
        instance: u64,
    },
}

#[derive(Clone)]
pub struct Pending {
    step: Step,
    relation: Relation,
    handle: Arc<RequestHandle>,
    target: Target,
}

fn ask(
    state: &AppContext,
    step: Step,
    relation: Relation,
    feature: Feature,
    spec: impl FnOnce(RequestSpec) -> RequestSpec,
    target: Target,
) {
    let Some(services) = services(state) else {
        return;
    };
    let handle = Arc::new(services.request(spec(RequestSpec::new(
        feature,
        target.path.clone(),
        target.lsp_stamp(),
        target.cursor,
    ))));
    state.update(|app| {
        app.editor.language.lists = Some(Pending {
            step,
            relation,
            handle,
            target,
        });
    });
}

/// Starts listing `relation` of the symbol at the caret.
pub fn hierarchy(state: &AppContext, relation: Relation) {
    let app = state.get();
    let pane = app.workspace.active_pane();
    let Some(t) = target(&app, pane) else { return };
    drop(app);
    ask(state, Step::Prepare, relation, relation.prepare(), |s| s, t);
}

/// Lists the code lenses of the file.
pub fn lenses(state: &AppContext) {
    let app = state.get();
    let pane = app.workspace.active_pane();
    let Some(t) = target(&app, pane) else { return };
    drop(app);
    ask(
        state,
        Step::Lenses,
        Relation::Callers,
        Feature::CodeLens,
        |s| s,
        t,
    );
}

fn why(what: &str, failure: Option<&Failure>) -> String {
    match failure {
        Some(Failure::Unsupported) => format!("No language server here can show {what}"),
        Some(Failure::Stale) => "The text changed; try again".to_owned(),
        Some(Failure::Failed(reason)) => format!("The language server failed: {reason}"),
        _ => format!("No {what} found"),
    }
}

pub fn resolve_lens(state: &AppContext, instance: u64, lens: Arc<reply::CodeLens>) {
    let app = state.get();
    let Some(t) = target(&app, app.workspace.active_pane()) else {
        return;
    };
    ask(
        state,
        Step::ResolveLens { instance },
        Relation::Callers,
        Feature::CodeLensResolve,
        |s| s.target(instance).extra(lens.raw.clone()),
        t,
    );
}

/// Takes the answer of the pending request when it is complete. May start the
/// next step, which is why it needs the application context.
pub fn tick(state: &AppContext, app: &mut AppState) {
    let Some(pending) = app.editor.language.lists.clone() else {
        return;
    };
    let progress = pending.handle.poll();
    if !progress.finished {
        return;
    }
    app.editor.language.lists = None;
    let t = &pending.target;
    let same_document = app.workspace.active_pane() == t.pane
        && app.workspace.pane(t.pane).and_then(|p| p.active()) == Some(t.view);
    if !same_document
        || target(app, t.pane).is_none_or(|now| {
            !now.same_view(t)
                || now.stamp != t.stamp
                || now.cursor != t.cursor
                || now.selection != t.selection
        })
    {
        return;
    }
    match pending.step {
        Step::Prepare => {
            let found = progress.usable(pending.relation.prepare()).first().cloned();
            let item = found.and_then(|part| {
                reply::hierarchy_items(&part.value, part.encoding)
                    .into_iter()
                    .next()
                    .map(|item| (part.instance, item))
            });
            match item {
                Some((instance, item)) => {
                    let next = state.clone();
                    let raw = item.raw.clone();
                    let target = t.clone();
                    let relation = pending.relation;
                    let item = Arc::new(item);
                    // The next request is made outside this update.
                    if let Some(application) = state.services().and_then(|r| r.application()) {
                        application.post(move || {
                            ask(
                                &next,
                                Step::Follow { instance, item },
                                relation,
                                relation.follow(),
                                |s| s.extra(raw).target(instance),
                                target,
                            )
                        });
                    }
                }
                None => app.show_toast(why("a symbol here", progress.failure.as_ref())),
            }
        }
        Step::Follow { instance, item } => {
            let mut rows = Vec::new();
            for part in &progress.parts {
                match pending.relation {
                    Relation::Callers | Relation::Callees => {
                        let side = if pending.relation == Relation::Callers {
                            "from"
                        } else {
                            "to"
                        };
                        for call in reply::calls(&part.value, side, part.encoding) {
                            rows.push(hierarchy_row(
                                app,
                                instance,
                                &call.item,
                                call.sites.first().copied(),
                                pending.relation,
                            ));
                        }
                    }
                    _ => {
                        for related in reply::hierarchy_items(&part.value, part.encoding) {
                            rows.push(hierarchy_row(
                                app,
                                instance,
                                &related,
                                None,
                                pending.relation,
                            ));
                        }
                    }
                }
            }
            if rows.is_empty() {
                app.show_toast(why("results", progress.failure.as_ref()));
            } else {
                app.editor.language.popup = Some(
                    Popup::new(Kind::Hierarchy, t.pane, t.stamp, Some(t.cursor), rows)
                        .with_field()
                        .titled(pending.relation.title(&item.name)),
                );
            }
        }
        Step::Lenses => {
            let mut rows = Vec::new();
            for part in &progress.parts {
                for lens in reply::code_lenses(&part.value, part.encoding) {
                    let line = lens.span.start.line as usize + 1;
                    rows.push(Item {
                        label: lens
                            .command
                            .as_ref()
                            .map_or_else(|| "Resolve code lens…".into(), |c| c.title.clone()),
                        detail: format!("line {line}"),
                        badge: "\u{25B6}",
                        faint: false,
                        payload: match lens.command.clone() {
                            Some(command) => Payload::Command {
                                instance: part.instance,
                                command,
                            },
                            None => Payload::Lens {
                                instance: part.instance,
                                lens: Arc::new(lens),
                            },
                        },
                    });
                }
            }
            if rows.is_empty() {
                app.show_toast(why("code lenses", progress.failure.as_ref()));
            } else {
                app.editor.language.popup = Some(
                    Popup::new(Kind::CodeLenses, t.pane, t.stamp, None, rows)
                        .with_field()
                        .titled("Code lenses"),
                );
            }
        }
        Step::ResolveLens { instance } => {
            let command = progress
                .parts
                .first()
                .and_then(|part| {
                    reply::code_lenses(&serde_json::json!([part.value.clone()]), part.encoding)
                        .into_iter()
                        .next()
                })
                .and_then(|lens| lens.command);
            if let Some(command) = command {
                if let Some(application) = state.services().and_then(|r| r.application()) {
                    let next = state.clone();
                    let source = t.clone();
                    application.post(move || {
                        let app = next.get();
                        let current = app.workspace.active_pane() == source.pane
                            && target(&app, source.pane).is_some_and(|now| {
                                now.same_view(&source)
                                    && now.stamp == source.stamp
                                    && now.cursor == source.cursor
                                    && now.selection == source.selection
                            });
                        drop(app);
                        if current {
                            let rect = next
                                .get()
                                .editor
                                .language
                                .rect
                                .unwrap_or(lgui::prelude::UiRect::new(0.0, 0.0, 0.0, 0.0));
                            super::refactor::execute(&next, instance, command, rect);
                        }
                    });
                }
            } else {
                app.show_toast(why("a code lens command", progress.failure.as_ref()));
            }
        }
    }
}

fn hierarchy_row(
    app: &AppState,
    _instance: u64,
    item: &HierarchyItem,
    site: Option<reply::Span>,
    relation: Relation,
) -> Item {
    let path = uri::file_path(&item.uri).unwrap_or_default();
    let place = match (relation, site) {
        // A caller is shown at the call, not at its own definition.
        (Relation::Callers, Some(site)) => Place::span(path.clone(), site),
        _ => Place::span(path.clone(), item.selection),
    };
    Item {
        label: item.name.clone(),
        detail: format!(
            "{}:{}",
            display_path(app, &path),
            item.selection.start.line + 1
        ),
        badge: super::actions::symbol_badge(item.kind),
        payload: Payload::Place(place),
        faint: false,
    }
}

/// Every problem the servers reported for the open files, most serious first.
pub fn problems(state: &AppContext) {
    let Some(services) = services(state) else {
        return;
    };
    let app = state.get();
    let pane = app.workspace.active_pane();
    let Some(t) = target(&app, pane) else { return };
    let mut found: Vec<(Severity, String, usize, Item)> = Vec::new();
    for (id, doc) in &app.workspace.documents {
        if !doc.is_text() {
            continue;
        }
        let Some(list) = services
            .results(&doc.meta.path, &doc.stamp(*id))
            .diagnostics
        else {
            continue;
        };
        let text = doc.buffer.text_snapshot();
        for d in list.iter() {
            let line = text[..d.range.start.min(text.len())].matches('\n').count();
            let name = display_path(&app, &doc.meta.path);
            found.push((
                d.severity,
                name.clone(),
                line,
                Item {
                    label: d.message.lines().next().unwrap_or_default().to_owned(),
                    detail: format!("{name}:{}", line + 1),
                    badge: match d.severity {
                        Severity::Error => "E",
                        Severity::Warning => "W",
                        Severity::Information => "I",
                        Severity::Hint => "H",
                    },
                    faint: d.severity == Severity::Hint,
                    payload: Payload::Place(Place::bytes(doc.meta.path.clone(), d.range.clone())),
                },
            ));
        }
    }
    found.sort_by(|a, b| (a.0, &a.1, a.2).cmp(&(b.0, &b.1, b.2)));
    let count = found.len();
    let mut groups: Vec<(std::path::PathBuf, Vec<Item>)> = Vec::new();
    let mut indices = std::collections::HashMap::new();
    for (.., item) in found {
        let Payload::Place(place) = &item.payload else {
            continue;
        };
        let path = place.path.clone();
        let at = *indices.entry(path.clone()).or_insert_with(|| {
            groups.push((path, Vec::new()));
            groups.len() - 1
        });
        groups[at].1.push(item);
    }
    let mut rows = Vec::new();
    for (path, items) in groups {
        rows.push(Item {
            label: display_path(&app, &path),
            detail: format!("{} problems", items.len()),
            badge: "▾",
            faint: false,
            payload: Payload::Group(path),
        });
        rows.extend(items);
    }
    drop(app);
    state.update(|app| {
        if count == 0 {
            app.show_toast("No problems in the open files");
            return;
        }
        let mut panel = Popup::new(Kind::Problems, t.pane, t.stamp, None, rows)
            .with_field()
            .titled(format!("{count} problems"));
        panel.resize(app.editor.language.rect.map_or(600.0, |r| r.height()));
        panel.select(1);
        app.editor.language.popup = Some(panel);
    });
}
