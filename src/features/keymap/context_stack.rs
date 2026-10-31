//! Pure key context rules from application-projected focus facts.
//!
use super::context::KeyContext;
#[derive(Clone)]
pub struct ContextFacts {
    pub surface: &'static str,
    pub extension: Option<String>,
    pub vim_mode: &'static str,
    pub close: bool,
    pub clone: bool,
    pub stash: bool,
    pub confirm: bool,
    pub branch: bool,
    pub palette: bool,
    pub commit: bool,
    pub search: bool,
    pub search_results: bool,
    pub other_input: bool,
    pub terminal: bool,
    pub editor: bool,
    pub vim_active: bool,
    pub menu: bool,
    /// A list is open over the code (completion, definitions, symbols).
    pub popup: bool,
    /// A snippet is being filled in.
    pub snippet: bool,
    pub snippet_choice: bool,
    pub signature: bool,
    pub language_peek: bool,
}

pub const WORKSPACE: &str = "Workspace";
pub const EDITOR: &str = "Editor";
pub const TERMINAL: &str = "Terminal";
pub const INPUT: &str = "Input";
pub const MENU: &str = "Menu";
pub const DIALOG: &str = "Dialog";
pub const CLOSE_CONFIRMATION: &str = "CloseConfirmation";
pub const CLONE_REPOSITORY: &str = "CloneRepository";
pub const BRANCH_PICKER: &str = "BranchPicker";
pub const COMMAND_PALETTE: &str = "CommandPalette";
pub const STASH_SWITCH: &str = "StashSwitch";
pub const GIT_CONFIRM: &str = "GitConfirm";
pub const COMMIT_MESSAGE: &str = "CommitMessage";
pub const VIM: &str = "Vim";
pub const POPUP: &str = "Popup";
pub const SNIPPET: &str = "Snippet";
pub const SNIPPET_CHOICE: &str = "SnippetChoice";
pub const SIGNATURE_HELP: &str = "SignatureHelp";

pub fn os_name() -> &'static str {
    if cfg!(target_os = "macos") {
        "macos"
    } else if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "linux") {
        "linux"
    } else {
        "unknown"
    }
}

/// The workspace root node, as menus without a focused child see it.
pub fn workspace_context(app: &ContextFacts) -> KeyContext {
    let mut workspace = KeyContext::new(WORKSPACE);
    workspace.set("os", os_name());
    workspace.set("surface", app.surface);
    workspace
}

pub fn editor_context(app: &ContextFacts) -> KeyContext {
    let mut editor = KeyContext::new(EDITOR);
    if let Some(extension) = app.extension.as_deref() {
        editor.set("extension", &extension.to_ascii_lowercase());
    }
    editor
}

/// The Vim node below the editor, e.g. `Vim mode=normal`.
pub fn vim_context(app: &ContextFacts) -> KeyContext {
    let mut vim = KeyContext::new(VIM);
    vim.set("mode", app.vim_mode);
    vim
}

/// The context stack from the root to the focused element.
pub fn context_stack(app: &ContextFacts) -> Vec<KeyContext> {
    // Modal dialogs are roots of their own, so workspace bindings never reach
    // through them.
    if app.close {
        let mut dialog = KeyContext::new(DIALOG);
        dialog.add(CLOSE_CONFIRMATION);
        return vec![dialog];
    }
    if app.clone {
        let mut dialog = KeyContext::new(DIALOG);
        dialog.add(CLONE_REPOSITORY);
        return vec![dialog];
    }
    if app.stash {
        let mut dialog = KeyContext::new(DIALOG);
        dialog.add(STASH_SWITCH);
        return vec![dialog];
    }
    if app.confirm {
        let mut dialog = KeyContext::new(DIALOG);
        dialog.add(GIT_CONFIRM);
        return vec![dialog];
    }
    // The palette's filter takes typing, so workspace shortcuts stay out.
    if app.palette {
        let mut dialog = KeyContext::new(DIALOG);
        dialog.add(COMMAND_PALETTE);
        return vec![dialog];
    }
    // The picker's filter takes typing, so workspace shortcuts stay out.
    if app.branch {
        let mut dialog = KeyContext::new(DIALOG);
        dialog.add(BRANCH_PICKER);
        return vec![dialog];
    }

    let mut stack = vec![workspace_context(app)];
    if app.commit {
        let mut input = KeyContext::new(INPUT);
        input.add(COMMIT_MESSAGE);
        stack.push(input);
    } else if app.search {
        let mut input = KeyContext::new(INPUT);
        input.add(if app.search_results {
            "SearchResults"
        } else {
            "SearchInput"
        });
        stack.push(input);
    } else if app.other_input {
        stack.push(KeyContext::new(INPUT));
    } else if app.terminal {
        stack.push(KeyContext::new(TERMINAL));
    } else if app.surface == "markdown" && app.editor {
        stack.push(KeyContext::new("Markdown"));
    } else if app.surface == "diff" && app.editor {
        stack.push(KeyContext::new("Diff"));
    } else if app.editor {
        stack.push(editor_context(app));
        if app.vim_active {
            stack.push(vim_context(app));
        }
        if app.snippet {
            stack.push(KeyContext::new(SNIPPET));
        }
        if app.snippet_choice {
            stack.push(KeyContext::new(SNIPPET_CHOICE));
        }
        if app.signature {
            stack.push(KeyContext::new(SIGNATURE_HELP));
        }
        if app.language_peek {
            stack.push(KeyContext::new("LanguagePeek"));
        }
        if app.popup {
            stack.push(KeyContext::new(POPUP));
        }
    }
    if app.menu {
        stack.push(KeyContext::new(MENU));
    }
    stack
}

/// Contexts joined for display, e.g. `Workspace os=windows > Editor`.
pub fn describe(stack: &[KeyContext]) -> String {
    stack
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join(" > ")
}
