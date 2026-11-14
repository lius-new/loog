//! The question whether to trust an opened folder (design §9.1).
//!
//! Every open folder nobody has decided on is asked about once, one at a time, and
//! the answer is remembered, a "no" included. What is asked about is always derived
//! from the stored decisions, so answering is what ends the question.
use crate::app::context::AppContext;
use crate::app::state::AppState;
use crate::features::extensions::trust::Decision;
use crate::features::keymap::action::Action;
use crate::infrastructure::extensions::trust_store::TrustStore;
use lgui::core::{UiFocusHandle, UiId};
use lgui::prelude::{Element, UiRect};
use loog_workspace_api::{Capability, ResourceRef};
use std::path::{Path, PathBuf};
use std::sync::Arc;

pub(crate) fn question(app: &AppState) -> Option<ResourceRef> {
    app.chrome
        .trust_question()
        .map(ResourceRef::local)
        .or_else(|| app.explorer.resources.trust_review.clone())
        .or_else(|| app.explorer.resources.trust_prompt.clone())
}

/// The folder to ask about now: the first open folder nobody has decided on, else
/// the folder of a file that is open outside every folder, when an extension
/// declares a language server for the file (a stray text file is not asked about).
fn pending_folder(
    app: &AppState,
    trust: &TrustStore,
    serves: impl Fn(&str) -> bool,
) -> Option<PathBuf> {
    if !trust.ready() {
        return None;
    }
    trust
        .undecided()
        .into_iter()
        .next()
        .or_else(|| loose_directory(app, trust, serves))
}

fn loose_directory(
    app: &AppState,
    trust: &TrustStore,
    serves: impl Fn(&str) -> bool,
) -> Option<PathBuf> {
    let id = app.workspace.active_document()?;
    let document = app.workspace.documents.get(&id).filter(|d| d.is_text())?;
    let path = document.meta.local_path()?;
    if app
        .explorer
        .model
        .roots()
        .iter()
        .any(|root| path.starts_with(root))
    {
        return None;
    }
    let directory = path.parent()?;
    if trust.decision_of(directory) != Decision::Undecided {
        return None;
    }
    let language = document.meta.lang.definition();
    serves(&language.name).then(|| directory.to_path_buf())
}

/// Keeps trust judged against the open folders and the question in step with
/// what is decided. Runs after every state change.
pub(crate) fn sync(app: &mut AppState, trust: &TrustStore, serves: impl Fn(&str) -> bool) {
    trust.set_workspace(app.explorer.model.roots());
    app.explorer.resources.trust_prompt = trust
        .ready()
        .then(|| {
            app.explorer
                .resources
                .roots
                .iter()
                .find(|root| {
                    root.descriptor
                        .capabilities
                        .iter()
                        .any(|cap| matches!(cap, Capability::Process | Capability::Terminal))
                        && trust.resource_decision(&root.descriptor.resource) == Decision::Undecided
                })
                .map(|root| root.descriptor.resource.clone())
        })
        .flatten();
    if app
        .explorer
        .resources
        .trust_review
        .as_ref()
        .is_some_and(|resource| {
            !app.explorer
                .resources
                .roots
                .iter()
                .any(|root| &root.descriptor.resource == resource)
        })
    {
        app.explorer.resources.trust_review = None;
    }
    let pending = pending_folder(app, trust, serves);
    if app.chrome.trust_prompt != pending {
        app.chrome.trust_prompt = pending;
    }
    // A second look at a folder ends when the folder is closed.
    if let Some(folder) = &app.chrome.trust_review
        && !app.explorer.model.roots().contains(folder)
    {
        app.chrome.trust_review = None;
    }
}

/// What the title bar says while open folders are declined: how many, and what
/// clicking it does. Nothing while a question is showing, which says it already.
pub(crate) fn restricted(app: &AppState, trust: &TrustStore) -> Option<(usize, String)> {
    if question(app).is_some() {
        return None;
    }
    let declined = trust.declined_open();
    let resource_declined: Vec<_> = app
        .explorer
        .resources
        .roots
        .iter()
        .filter(|root| trust.resource_decision(&root.descriptor.resource) == Decision::Declined)
        .collect();
    if !resource_declined.is_empty() {
        let count = declined.len() + resource_declined.len();
        let label = if count == 1 {
            format!(
                "{} is restricted. Decide again",
                resource_declined[0].descriptor.name
            )
        } else {
            format!("{count} folders are restricted. Decide again")
        };
        return Some((count, label));
    }
    let first = declined.first()?;
    let name = |path: &Path| {
        path.file_name().map_or_else(
            || path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        )
    };
    let label = if declined.len() == 1 {
        format!("{} is restricted. Decide again", name(first))
    } else {
        format!("{} folders are restricted. Decide again", declined.len())
    };
    Some((declined.len(), label))
}

/// Ask again about a folder that was declined: the one holding the active file,
/// else the first. Answering decides it for good, and the next one can be looked at.
pub(crate) fn review(state: &AppContext) {
    let Some(runtime) = state.services() else {
        return;
    };
    let declined = runtime.trust().declined_open();
    state.update(|app| {
        let resources: Vec<_> = app
            .explorer
            .resources
            .roots
            .iter()
            .filter(|root| {
                runtime.trust().resource_decision(&root.descriptor.resource) == Decision::Declined
            })
            .map(|root| root.descriptor.resource.clone())
            .collect();
        if !resources.is_empty() {
            let active = app
                .workspace
                .active_document()
                .and_then(|id| app.workspace.documents[&id].meta.provider_root.clone());
            app.explorer.resources.trust_review = active
                .filter(|root| resources.contains(root))
                .or_else(|| resources.first().cloned());
            return;
        }
        let active = app
            .workspace
            .active_path()
            .and_then(|path| app.workspace_folder_of(path))
            .map(Path::to_path_buf)
            .filter(|folder| declined.contains(folder));
        app.chrome.trust_review = active.or_else(|| declined.first().cloned());
    });
}

/// Record the answer for `root` and carry it out: a trusted folder gets its
/// servers started, a declined one stops what runs there.
pub(crate) fn decide(state: &AppContext, root: &Path, trust: bool) {
    let Some(runtime) = state.services() else {
        return;
    };
    // The decision holds in memory even when saving it fails, so the question ends either way.
    let saved = if trust {
        runtime.trust().trust(root)
    } else {
        runtime.trust().decline(root)
    };
    if let Some(host) = runtime.extensions() {
        if trust {
            host.resume_tools_in(state, root);
        } else {
            host.stop_tools_in(root);
        }
    }
    // Nothing is announced on success: the question simply ends. Only a failure to
    // save is reported. The update is also what lets the next folder be asked about.
    state.update(|app| {
        app.chrome.trust_review = None;
        if let Err(error) = saved {
            app.show_error(format!("Could not save the trust decision: {error}"));
        }
    });
}

fn answer(state: &AppContext, trust: bool) -> bool {
    let Some(root) = question(&state.get()) else {
        return false;
    };
    if let Some(path) = root.local_path() {
        decide(state, path, trust);
    } else if let Some(runtime) = state.services() {
        let saved = runtime.trust().decide_resource(&root, trust);
        if let Some(host) = runtime.extensions() {
            host.refresh_resource_grants();
        }
        state.update(|app| {
            app.explorer.resources.trust_review = None;
            if let Err(error) = saved {
                app.show_error(format!("Could not save the trust decision: {error}"));
            }
        });
    }
    true
}

/// Enter trusts the folder; Escape is a "no", which is remembered.
pub fn perform(state: &AppContext, editor_focus: &UiFocusHandle, action: Action) -> bool {
    let trust = match action {
        Action::DialogConfirm => true,
        Action::DialogCancel | Action::DialogSecondary => false,
        _ => return false,
    };
    let answered = answer(state, trust);
    if answered {
        editor_focus.focus();
    }
    answered
}

fn question_label(app: &AppState) -> String {
    question(app)
        .map(|root| match root.local_path() {
            Some(path) => crate::ui::display_path(path),
            None => {
                let ResourceRef::Provided { path, .. } = &root else {
                    unreachable!()
                };
                app.explorer
                    .resources
                    .roots
                    .iter()
                    .find(|item| item.descriptor.resource == root)
                    .map(|item| format!("{} — {}", item.descriptor.name, path.as_str()))
                    .unwrap_or_else(|| path.as_str().to_owned())
            }
        })
        .unwrap_or_default()
}

pub fn render(rect: UiRect, state: AppContext, id: UiId, focus: UiFocusHandle) -> Element {
    let folder = question_label(&state.get());
    let on_answer = state.clone();
    crate::ui::trust_prompt::render(
        rect,
        folder,
        Arc::new(move |trust| {
            answer(&on_answer, trust);
        }),
        id,
        focus,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn resource_questions_hide_opaque_ids_and_review_both_trust_decisions() {
        let trust = store("resource-question");
        let resource = ResourceRef::provided(
            loog_workspace_api::EnvironmentKey::new("example.provider", "opaque-config").unwrap(),
            "/work",
        )
        .unwrap();
        let mut app = AppState::new();
        app.explorer
            .resources
            .sync(vec![loog_workspace_api::PresentedRoot {
                scope: 1,
                resource: resource.clone(),
                name: "Development".into(),
                capabilities: vec![Capability::FilesRead, Capability::Terminal],
                badge: None,
            }]);
        sync(&mut app, &trust, |_| false);
        assert_eq!(question(&app), Some(resource.clone()));
        assert_eq!(question_label(&app), "Development — /work");
        trust.decide_resource(&resource, false).unwrap();
        sync(&mut app, &trust, |_| false);
        assert!(question(&app).is_none());
        assert_eq!(restricted(&app, &trust).unwrap().0, 1);
        trust.decide_resource(&resource, true).unwrap();
        app.explorer.resources.trust_review = Some(resource.clone());
        sync(&mut app, &trust, |_| false);
        assert_eq!(question(&app), Some(resource));
        app.explorer.resources.sync(vec![]);
        sync(&mut app, &trust, |_| false);
        assert!(question(&app).is_none());
    }

    fn store(tag: &str) -> TrustStore {
        let store = TrustStore::default();
        store.attach(std::env::temp_dir().join(format!(
            "loog-trust-prompt-{}-{tag}/roots.json",
            std::process::id()
        )));
        store
    }

    fn open(app: &mut AppState, folder: &Path) {
        app.explorer.model.add_root(
            &crate::infrastructure::explorer_filesystem::ExplorerFilesystem,
            folder.to_path_buf(),
        );
    }

    #[test]
    fn each_open_folder_is_asked_about_in_turn_and_a_no_is_not_asked_again() {
        let base = std::env::temp_dir().join(format!("loog-prompt-roots-{}", std::process::id()));
        let (one, two) = (base.join("one"), base.join("two"));
        std::fs::create_dir_all(&one).unwrap();
        std::fs::create_dir_all(&two).unwrap();
        let trust = store("turns");
        let mut app = AppState::new();
        open(&mut app, &one);
        open(&mut app, &two);

        sync(&mut app, &trust, |_| false);
        let first = app
            .chrome
            .trust_prompt
            .clone()
            .expect("asks about a folder");
        assert!(first.ends_with("one"), "{first:?}");

        trust.trust(&first).unwrap();
        sync(&mut app, &trust, |_| false);
        let second = app.chrome.trust_prompt.clone().expect("then the next one");
        assert!(second.ends_with("two"), "{second:?}");

        trust.decline(&second).unwrap();
        sync(&mut app, &trust, |_| false);
        assert_eq!(app.chrome.trust_prompt, None, "both answered");
        // The same folders on the next start are not asked about again.
        sync(&mut app, &trust, |_| false);
        assert_eq!(app.chrome.trust_prompt, None);
    }

    #[test]
    fn nothing_is_asked_before_the_stored_decisions_are_read() {
        let base = std::env::temp_dir().join(format!("loog-prompt-early-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let trust = TrustStore::default();
        let mut app = AppState::new();
        open(&mut app, &base);
        sync(&mut app, &trust, |_| true);
        assert_eq!(app.chrome.trust_prompt, None);
    }

    #[test]
    fn declined_folders_are_noted_until_trusted_and_not_while_a_question_shows() {
        let base = std::env::temp_dir().join(format!("loog-prompt-note-{}", std::process::id()));
        let (one, two) = (base.join("one"), base.join("two"));
        std::fs::create_dir_all(&one).unwrap();
        std::fs::create_dir_all(&two).unwrap();
        let trust = store("note");
        let mut app = AppState::new();
        open(&mut app, &one);
        open(&mut app, &two);
        sync(&mut app, &trust, |_| false);
        assert!(
            restricted(&app, &trust).is_none(),
            "a folder being asked about is not noted"
        );

        let (first, second) = (
            app.explorer.model.roots()[0].clone(),
            app.explorer.model.roots()[1].clone(),
        );
        trust.decline(&first).unwrap();
        trust.decline(&second).unwrap();
        sync(&mut app, &trust, |_| false);
        let (count, label) = restricted(&app, &trust).expect("both are noted");
        assert_eq!(count, 2);
        assert!(label.contains("2 folders"), "{label}");

        // Looking again hides the note; trusting one leaves the other noted.
        app.chrome.trust_review = Some(first.clone());
        assert!(restricted(&app, &trust).is_none());
        trust.trust(&first).unwrap();
        app.chrome.trust_review = None;
        let (count, label) = restricted(&app, &trust).unwrap();
        assert_eq!(count, 1);
        assert!(label.starts_with("two "), "{label}");

        // Closing the folder that is looked at again ends the second look.
        app.chrome.trust_review = Some(second.clone());
        app.explorer.model.remove_root(&second);
        sync(&mut app, &trust, |_| false);
        assert_eq!(app.chrome.trust_review, None);
    }

    #[test]
    fn the_question_goes_away_when_its_folder_is_closed() {
        let base = std::env::temp_dir().join(format!("loog-prompt-close-{}", std::process::id()));
        std::fs::create_dir_all(&base).unwrap();
        let trust = store("close");
        let mut app = AppState::new();
        open(&mut app, &base);
        sync(&mut app, &trust, |_| false);
        assert!(app.chrome.trust_prompt.is_some());
        let root = app.explorer.model.roots()[0].clone();
        app.explorer.model.remove_root(&root);
        sync(&mut app, &trust, |_| false);
        assert_eq!(app.chrome.trust_prompt, None);
    }
}
