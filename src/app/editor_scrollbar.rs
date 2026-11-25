//! Reveals an editor's scrollbars while its text scrolls without the pointer
//! over it, such as when the keyboard moves the caret far enough to scroll.
use std::sync::Arc;
use std::time::Duration;

use crate::app::context::AppContext;
use crate::app::state::AppState;
use crate::app::ui_timer::{self, Timer};
use crate::domain::identity::{PaneId, ViewId};

/// How long the scrollbars stay after the last scroll.
const REVEAL: Duration = Duration::from_millis(1200);

#[derive(Clone, Default)]
pub struct Reveal {
    seen: Option<(PaneId, Option<ViewId>, f32, f32)>,
    /// The pane whose scrollbars show for now.
    pub shown: Option<PaneId>,
    timer: Option<Arc<Timer>>,
}

/// Runs after every state update. Switching documents or panes never reveals.
pub(crate) fn observe(ctx: &AppContext, app: &mut AppState) {
    let pane = app.workspace.active_pane();
    let view = app.workspace.pane(pane).and_then(|pane| pane.active());
    let (x, y) = app.workspace.active_scroll();
    let now = (pane, view, x, y);
    let previous = app.editor.scrollbar_reveal.seen.replace(now);
    let Some((old_pane, old_view, old_x, old_y)) = previous else {
        return;
    };
    if old_pane != pane || old_view != view || (old_x == x && old_y == y) {
        return;
    }
    // The pointer over the pane or a dragged thumb already shows them.
    let dragging = app.editor.editor_vertical_scrollbar_dragging
        || app.editor.editor_horizontal_scrollbar_dragging;
    if !app.editor.focused || app.editor.editor_hovered == Some(pane) || dragging {
        return;
    }
    let Some(application) = ctx.services().and_then(|runtime| runtime.application()) else {
        return;
    };
    let timed = ctx.clone();
    app.editor.scrollbar_reveal.shown = Some(pane);
    app.editor.scrollbar_reveal.timer = Some(Arc::new(ui_timer::after(application, REVEAL, move || {
        timed.update(|app| {
            app.editor.scrollbar_reveal.shown = None;
            app.editor.scrollbar_reveal.timer = None;
        });
    })));
}
