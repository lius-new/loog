//! Repository clone orchestration and destination selection.
use crate::app::context::AppContext;
use crate::features::git::clone_url::repository_directory_name;
use lgui::dialogs::{FileDialogOptions, system_file_dialogs};
use std::thread;
pub fn clone_repository(state: &AppContext) -> bool {
    let url = state.get().git.ui.clone_url.trim().to_owned();
    if url.is_empty() {
        state.update(|app| {
            app.git.ui.clone_error = Some("Enter a repository URL.".to_owned());
        });
        return false;
    }
    let Some(directory_name) = repository_directory_name(&url) else {
        state.update(|app| {
            app.git.ui.clone_error = Some("The repository URL has no project name.".to_owned());
        });
        return false;
    };

    let mut options = FileDialogOptions::new().title("Select Repository Location");
    if let Some(directory) = state.get().explorer.model.roots().first().cloned() {
        options = options.directory(directory);
    }
    let Some(parent) = system_file_dialogs().pick_folder(&options) else {
        return false;
    };
    let target = parent.join(directory_name);
    if crate::infrastructure::git::repo_fs::exists(&target) {
        let message = format!("{} already exists.", target.display());
        state.update(move |app| app.git.ui.clone_error = Some(message));
        return false;
    }

    state.update(|app| {
        app.git.model.begin_clone();
        app.git.ui.clone_error = None;
    });
    let clone_state = state.clone();
    thread::Builder::new()
        .name("loog-git-clone".to_owned())
        .spawn(move || {
            let result = crate::infrastructure::git::service().and_then(|service| {
                service.clone_repository(
                    &url,
                    &target,
                    &crate::infrastructure::git::command::CancellationToken::default(),
                )
            });

            clone_state.update(move |app| {
                app.git.model.finish_clone();
                match result {
                    Ok(()) => {
                        app.chrome.toast = None;
                        crate::app::explorer::load_folder(app, target);
                        app.git.ui.clone_visible = false;
                        app.git.ui.clone_url.clear();
                        app.git.ui.clone_error = None;
                        app.git.ui.clone_input_focused = false;
                    }
                    Err(error) => {
                        app.git.ui.clone_error = Some(error.user_message());
                    }
                }
            });
        })
        .map(|_| true)
        .unwrap_or_else(|error| {
            state.update(move |app| {
                app.git.model.finish_clone();
                app.git.ui.clone_error = Some(format!("Could not start git: {error}"));
            });
            false
        })
}
