//! The Outline drawer's data: which document it describes, where the tree came
//! from (Markdown headings read here, or a language server's symbols), and what
//! the person has done to it (collapsed branches, sorting, scrolling).
use super::{AppContext, AppState, services, target};
use crate::app::ui_timer;
use crate::domain::document::DocumentStamp;
use crate::features::language_service::reply;
use crate::features::language_service::request::Feature;
use crate::features::outline::{self, Outline};
use crate::features::providers::{Failure, RequestHandle, RequestSpec};
use crate::ui::drawer::DrawerMode;
use loog_workspace_api::ResourceRef;
use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

/// How long typing pauses before the structure is read again.
const REST: Duration = Duration::from_millis(300);
/// Servers may still be starting when a file opens; ask again a few times.
const RETRY: Duration = Duration::from_millis(1200);
const RETRIES: u8 = 4;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum Status {
    /// The active tab is not a text document.
    #[default]
    NoDocument,
    Loading,
    Ready,
    Unavailable(String),
}

#[derive(Clone)]
struct Pending {
    handle: Arc<RequestHandle>,
    stamp: DocumentStamp,
    text: Arc<String>,
}

#[derive(Clone, Default)]
pub struct OutlineUi {
    pub status: Status,
    pub outline: Arc<Outline>,
    pub resource: Option<ResourceRef>,
    pub collapsed: HashSet<String>,
    pub sorted: bool,
    pub scroll: f32,
    /// Key of the node the view should bring into sight once.
    pub reveal: Option<String>,
    /// Key of the node holding the caret, as far as it is visible.
    pub current: Option<String>,
    pub hovered: Option<usize>,
    pub pointer_inside: bool,
    pub scrollbar_dragging: bool,
    pub scrollbar_drag_offset: f32,
    caret: Option<usize>,
    seen: Option<DocumentStamp>,
    retries: u8,
    timer: Option<Arc<ui_timer::Timer>>,
    pending: Option<Pending>,
}

impl OutlineUi {
    fn follow(&mut self) {
        let key = self
            .caret
            .and_then(|caret| self.outline.containing(caret))
            .map(|node| self.outline.visible_stand_in(node, &self.collapsed))
            .and_then(|node| self.outline.node(node))
            .map(|node| node.key.clone());
        if key != self.current {
            self.reveal = key.clone();
            self.current = key;
        }
    }

    pub fn toggle(&mut self, key: &str) {
        if !self.collapsed.remove(key) {
            self.collapsed.insert(key.to_owned());
        }
        self.follow();
    }

    pub fn expand_all(&mut self) {
        self.collapsed.clear();
        self.follow();
    }

    pub fn collapse_all(&mut self) {
        self.collapsed = self.outline.branch_keys();
        self.follow();
    }

    pub fn toggle_sort(&mut self) {
        self.sorted = !self.sorted;
        self.reveal = self.current.clone();
    }
}

fn visible(app: &AppState) -> bool {
    app.drawer.visible && app.drawer.mode == DrawerMode::Outline
}

/// Runs after every update: when the drawer is showing, keeps the tree in step
/// with the active document and the caret.
pub fn observe(ctx: &AppContext, app: &mut AppState) {
    if !visible(app) {
        let ui = &mut app.editor.language.outline;
        if ui.seen.is_some() || ui.timer.is_some() || ui.pending.is_some() {
            ui.seen = None;
            ui.timer = None;
            ui.pending = None;
        }
        return;
    }
    let pane = app.workspace.active_pane();
    let Some(t) = target(app, pane) else {
        let ui = &mut app.editor.language.outline;
        if ui.status != Status::NoDocument {
            *ui = OutlineUi {
                sorted: ui.sorted,
                ..OutlineUi::default()
            };
        }
        return;
    };
    let ui = &mut app.editor.language.outline;
    ui.caret = Some(t.cursor);
    if ui.seen == Some(t.stamp) {
        ui.follow();
        return;
    }
    let same_file = ui.resource.as_ref() == Some(&t.resource);
    ui.seen = Some(t.stamp);
    ui.pending = None;
    ui.timer = None;
    ui.retries = 0;
    if !same_file {
        *ui = OutlineUi {
            status: Status::Loading,
            sorted: ui.sorted,
            resource: Some(t.resource.clone()),
            caret: ui.caret,
            seen: ui.seen,
            ..OutlineUi::default()
        };
        refresh(ctx, app);
    } else {
        schedule(ctx, app, REST);
    }
}

fn schedule(ctx: &AppContext, app: &mut AppState, delay: Duration) {
    let Some(application) = ctx.services().and_then(|runtime| runtime.application()) else {
        return;
    };
    let timed = ctx.clone();
    app.editor.language.outline.timer =
        Some(Arc::new(ui_timer::after(application, delay, move || {
            timed.update(|app| refresh(&timed, app))
        })));
}

/// Reads the structure of the active document now.
fn refresh(ctx: &AppContext, app: &mut AppState) {
    if !visible(app) {
        return;
    }
    let pane = app.workspace.active_pane();
    let Some(t) = target(app, pane) else { return };
    if app.editor.language.outline.seen != Some(t.stamp) {
        return;
    }
    let ui = &mut app.editor.language.outline;
    ui.timer = None;
    if app.workspace.documents[&t.document]
        .meta
        .lang
        .definition()
        .name
        .as_ref()
        == "markdown"
    {
        ui.outline = Arc::new(Outline::new(outline::markdown::items(&t.text)));
        ui.status = Status::Ready;
        ui.follow();
        return;
    }
    let Some(services) = services(ctx) else {
        unavailable(ui);
        return;
    };
    let handle = Arc::new(services.request(RequestSpec::new(
        Feature::DocumentSymbol,
        t.resource.clone(),
        t.lsp_stamp(),
        t.cursor,
    )));
    ui.pending = Some(Pending {
        handle,
        stamp: t.stamp,
        text: t.text.clone(),
    });
}

fn unavailable(ui: &mut OutlineUi) {
    if ui.outline.is_empty() {
        ui.status = Status::Unavailable("No structure is available for this file.".to_owned());
    }
}

/// Takes the servers' answer when it is complete.
pub fn tick(ctx: &AppContext, app: &mut AppState) {
    let Some(pending) = app.editor.language.outline.pending.clone() else {
        return;
    };
    let progress = pending.handle.poll();
    if !progress.finished {
        return;
    }
    let ui = &mut app.editor.language.outline;
    ui.pending = None;
    if ui.seen != Some(pending.stamp) {
        return;
    }
    let mut symbols = Vec::new();
    for part in progress.usable(Feature::DocumentSymbol) {
        symbols.extend(reply::symbols(&part.value, part.encoding));
    }
    if !symbols.is_empty() || progress.failure.is_none() {
        let items = outline::lsp::items(&symbols, &pending.text);
        ui.outline = Arc::new(Outline::new(items));
        ui.status = Status::Ready;
        ui.follow();
        return;
    }
    if ui.retries < RETRIES {
        ui.retries += 1;
        schedule(ctx, app, RETRY);
        return;
    }
    match progress.failure {
        Some(Failure::Failed(reason)) if ui.outline.is_empty() => {
            ui.status = Status::Unavailable(format!("The language server failed: {reason}"));
        }
        _ => unavailable(ui),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::features::outline::{Item, Kind};

    fn ui_with(items: Vec<Item>) -> OutlineUi {
        OutlineUi {
            outline: Arc::new(Outline::new(items)),
            status: Status::Ready,
            ..OutlineUi::default()
        }
    }

    fn item(name: &str, range: std::ops::Range<usize>, children: Vec<Item>) -> Item {
        Item {
            name: name.to_owned(),
            detail: None,
            kind: Kind::Function,
            deprecated: false,
            select: range.start..range.start,
            full: range,
            children,
        }
    }

    #[test]
    fn following_the_caret_marks_the_node_and_asks_to_reveal_it_once() {
        let mut ui = ui_with(vec![item("a", 0..20, vec![item("b", 5..10, vec![])])]);
        ui.caret = Some(7);
        ui.follow();
        let key = ui.current.clone().unwrap();
        assert!(key.ends_with("Function:b"));
        assert_eq!(ui.reveal.as_deref(), Some(key.as_str()));
        ui.reveal = None;
        ui.follow();
        assert_eq!(ui.reveal, None);
    }

    #[test]
    fn collapsing_a_parent_moves_the_mark_to_it() {
        let mut ui = ui_with(vec![item("a", 0..20, vec![item("b", 5..10, vec![])])]);
        ui.caret = Some(7);
        ui.follow();
        let parent = ui.outline.node(0).unwrap().key.clone();
        ui.toggle(&parent);
        assert_eq!(ui.current.as_deref(), Some(parent.as_str()));
        ui.expand_all();
        assert!(ui.current.as_deref().unwrap().ends_with("Function:b"));
    }
}
