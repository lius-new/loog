//! Commands for working on highlight queries: see how the code at the cursor is
//! coloured and by which rule, and list custom queries that were left out.
use super::context::AppContext;
use crate::domain::language::Registry;
use crate::features::syntax::{MAX_DOCUMENT_BYTES, inspect};
use crate::infrastructure::language_packs;

/// Explain the colour at the caret of the active file as a message. The parse
/// runs on a background thread; only the message comes back to the UI thread.
pub(crate) fn inspect_cursor(state: &AppContext) {
    let background = state.clone();
    state.update(move |app| {
        let target = app
            .workspace
            .active_document()
            .and_then(|id| app.workspace.documents.get(&id).filter(|d| d.is_text()))
            .map(|document| (document.meta.lang, document.buffer.text_snapshot()));
        let Some((lang, text)) = target else {
            app.show_toast("Open a text file to inspect its highlighting.");
            return;
        };
        let Some(grammar) = lang.grammar else {
            app.show_toast(format!(
                "{} has no syntax grammar, so there are no query rules to inspect.",
                lang.display_name()
            ));
            return;
        };
        if text.len() > MAX_DOCUMENT_BYTES {
            app.show_toast("This file is too large to be parsed, so it has no query rules.");
            return;
        }
        let offset = app
            .workspace
            .active_editor()
            .map_or(0, |editor| editor.cursor());
        let name = Registry::builtin().grammar_name(grammar).to_owned();
        std::thread::spawn(move || {
            let resources = language_packs::resources();
            let report = inspect::inspect(resources.as_ref(), &name, &text, offset);
            background.update(move |app| match report {
                Ok(found) => app.show_toast(inspect::describe(&found, |class| {
                    format!("#{:06x}", crate::ui::theme::c().syntax_color(class).0)
                })),
                Err(error) => app.show_error(format!("Could not inspect highlighting: {error}")),
            });
        });
    });
}

/// List the custom query layers that were left out, and where to put your own.
pub(crate) fn show_problems(state: &AppContext) {
    let problems = language_packs::resources().query_problems();
    let folder = language_packs::user_resource_directory("queries")
        .map(|path| path.display().to_string())
        .unwrap_or_else(|| "(no user folder)".to_owned());
    let message = if problems.is_empty() {
        format!(
            "No highlight queries were left out. Your own go in {folder}\\<grammar>\\highlights.scm; start the file with \"; extends\" to add to the grammar's rules."
        )
    } else {
        format!(
            "Highlight queries left out:\n{}\nYour own go in {folder}",
            problems.join("\n")
        )
    };
    state.update(move |app| {
        if problems.is_empty() {
            app.show_toast(message);
        } else {
            app.show_error(message);
        }
    });
}
