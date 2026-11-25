//! Animates the Git commit composer between its idle line and its open box.
//!
//! Frames run only while it moves: a short worker posts a handful of steps to
//! the UI thread and then ends, so a settled composer costs no redraws.
use std::time::Duration;

use crate::app::context::AppContext;
use crate::app::state::AppState;

const DURATION: Duration = Duration::from_millis(160);
const FRAME: Duration = Duration::from_millis(16);

/// Runs after every state update: starts the animation when the composer's
/// wanted state differs from the one it settled on.
pub(crate) fn observe(ctx: &AppContext, app: &mut AppState) {
    let ui = &mut app.git.ui;
    let wanted = crate::ui::git::panel::commit_expanded(ui);
    if wanted == ui.composer_shown {
        return;
    }
    let Some(application) = ctx.services().and_then(|runtime| runtime.application()) else {
        // Without a frame source the composer simply snaps.
        ui.composer_shown = wanted;
        ui.composer_t = None;
        return;
    };
    let from = ui
        .composer_t
        .unwrap_or(if ui.composer_shown { 1.0 } else { 0.0 });
    ui.composer_shown = wanted;
    ui.composer_t = Some(from);
    let ctx = ctx.clone();
    let steps = (DURATION.as_millis() / FRAME.as_millis()) as u32;
    let spawned = std::thread::Builder::new()
        .name("loog-git-composer".into())
        .spawn(move || {
            for _ in 0..=steps {
                std::thread::sleep(FRAME);
                let ctx = ctx.clone();
                application.post(move || ctx.update(|app| step(app, 1.0 / steps as f32)));
            }
        });
    if spawned.is_err() {
        ui.composer_t = None;
    }
}

fn step(app: &mut AppState, amount: f32) {
    let ui = &mut app.git.ui;
    let Some(t) = ui.composer_t else {
        return;
    };
    let next = if ui.composer_shown {
        t + amount
    } else {
        t - amount
    };
    ui.composer_t = (0.0 < next && next < 1.0).then_some(next);
}
