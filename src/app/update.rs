//! Update orchestration: periodic checks on a worker thread, and the title
//! bar button and Settings action that check, download, install or open a
//! release.
use crate::app::context::AppContext;
use crate::features::update::{Intent, Version};
use crate::infrastructure::update;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::thread;
use std::time::Duration;

/// Let startup settle before the first request.
const FIRST_CHECK: Duration = Duration::from_secs(10);
const CHECK_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// Check now and then every day while `enabled`; the returned closure stops
/// the worker. Builds without an update feed never check.
pub fn start(state: AppContext, enabled: bool) -> impl FnOnce() {
    let install = update::install_kind();
    state.update(|app| app.update.set_install_kind(install));
    let platform = update::platform();
    let (stop, stopped) = mpsc::channel::<()>();
    let worker = (enabled && install.is_some())
        .then_some(platform)
        .flatten()
        .and_then(|platform| {
            thread::Builder::new()
                .name("loog-update-check".to_owned())
                .spawn(move || {
                    let mut wait = FIRST_CHECK;
                    while let Err(RecvTimeoutError::Timeout) = stopped.recv_timeout(wait) {
                        wait = CHECK_INTERVAL;
                        if state.try_update(|app| app.update.begin_check()) {
                            finish_check(&state, platform);
                        }
                    }
                })
                .ok()
        });
    move || {
        let _ = stop.send(());
        // The worker may be inside a request; let it finish on its own.
        drop(worker);
    }
}

/// Run a check the phase already marks as `Checking`.
fn finish_check(state: &AppContext, platform: crate::features::update::Platform) {
    let result = update::check(platform);
    let current = Version::current();
    state.update(move |app| app.update.finish_check(result, &current));
}

/// Handle a click on the title bar update button or the Settings action. Returns true when Loog
/// should now close so the installer can run.
pub fn activate(state: &AppContext) -> bool {
    let mut intent = None;
    state.update(|app| intent = app.update.click());
    match intent {
        Some(Intent::Check) => {
            let worker = state.clone();
            let spawned = update::platform().and_then(|platform| {
                thread::Builder::new()
                    .name("loog-update-check".to_owned())
                    .spawn(move || finish_check(&worker, platform))
                    .ok()
            });
            if spawned.is_none() {
                let current = Version::current();
                state.update(move |app| {
                    app.update
                        .finish_check(Err("could not start the check".to_owned()), &current)
                });
            }
            false
        }
        Some(Intent::Download(asset)) => {
            let worker = state.clone();
            let spawned = thread::Builder::new()
                .name("loog-update-download".to_owned())
                .spawn(move || {
                    let progress = worker.clone();
                    let result = update::download(&asset, |received| {
                        progress.try_update(|app| app.update.download_progress(received));
                    });
                    worker.update(move |app| {
                        if let Err(error) = app.update.finish_download(result) {
                            app.show_error(format!("Could not download the update: {error}"));
                        }
                    });
                });
            if let Err(error) = spawned {
                state.update(move |app| {
                    let _ = app.update.finish_download(Err(error.to_string()));
                    app.show_error(format!("Could not download the update: {error}"));
                });
            }
            false
        }
        Some(Intent::OpenPage(url)) => {
            if let Err(error) = update::open_page(&url) {
                state.update(move |app| app.show_error(format!("Could not open {url}: {error}")));
            }
            false
        }
        Some(Intent::Install(installer)) => {
            update::install_on_exit(installer);
            true
        }
        None => false,
    }
}
