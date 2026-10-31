//! Every keyboard-reachable command, named the way keymap files refer to it
//! (`namespace::Name`).
//!
//! Actions are a closed enum rather than a dynamic registry: Loom has no
//! extensions, and the compiler then checks that every action is handled.

use serde_json::Value;

use loom_editor_core::commands::Command;
use loom_editor_core::normal::Movement;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ActionMeta {
    pub name: &'static str,
    pub description: &'static str,
    /// Whether the action is written `["name", argument]` in keymap files.
    pub takes_argument: bool,
}

macro_rules! actions {
    ($($variant:ident => $name:literal, $description:literal;)*) => {
        // `NoAction` keeps Zed's name for what `null` binds to.
        #[allow(clippy::enum_variant_names)]
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
        pub enum Action {
            $($variant,)*
            /// `["pane::ActivateItem", index]`: select a tab by position.
            ActivateItem(usize),
            /// `["vim::Keys", "<Esc>"]`: send keys to Vim, without remapping them.
            VimKeys(VimKeys),
            /// `["extension::Run", "vendor.name.command"]`: run an extension command.
            RunExtension(ExtensionCommand),
            /// `null` in a keymap file: disables the keystroke in that context.
            NoAction,
        }

        /// Actions that keymap files may name, in display order.
        pub const ACTIONS: &[ActionMeta] = &[
            $(ActionMeta { name: $name, description: $description, takes_argument: false },)*
            ActionMeta {
                name: "pane::ActivateItem",
                description: "Activate the tab at a zero-based position",
                takes_argument: true,
            },
            ActionMeta {
                name: "extension::Run",
                description: "Run a command contributed by an extension",
                takes_argument: true,
            },
            ActionMeta {
                name: "vim::Keys",
                description: "Send keys in Vim notation to Vim (a key mapping)",
                takes_argument: true,
            },
        ];

        fn unit_action(name: &str) -> Option<Action> {
            match name {
                $($name => Some(Action::$variant),)*
                _ => None,
            }
        }

        impl Action {
            pub fn name(self) -> &'static str {
                match self {
                    $(Action::$variant => $name,)*
                    Action::ActivateItem(_) => "pane::ActivateItem",
                    Action::VimKeys(_) => "vim::Keys",
                    Action::RunExtension(_) => "extension::Run",
                    Action::NoAction => "null",
                }
            }
        }
    };
}

actions! {
    ToggleDrawer => "workspace::ToggleDrawer", "Show or hide the Explorer drawer";
    OpenExplorer => "workspace::OpenExplorer", "Show the Explorer drawer";
    OpenSearch => "workspace::OpenSearch", "Search across workspace files";
    OpenOutline => "workspace::OpenOutline", "Show the structure of the current file";
    SearchNext => "search::SelectNextMatch", "Preview the next search match";
    SearchPrevious => "search::SelectPreviousMatch", "Preview the previous search match";
    SearchOpen => "search::OpenMatch", "Open the selected search match";
    SearchCancel => "search::Cancel", "Cancel workspace search";
    SearchRefresh => "search::Refresh", "Search the workspace again";
    SearchToggleCase => "search::ToggleCase", "Toggle case sensitive search";
    SearchToggleWord => "search::ToggleWholeWord", "Toggle whole word search";
    SearchToggleRegex => "search::ToggleRegex", "Toggle regular expression search";
    SearchToggleFilters => "search::ToggleFilters", "Show or hide search filters";
    OpenSourceControl => "workspace::OpenSourceControl", "Show the Source Control panel";
    ToggleSourceControl => "workspace::ToggleSourceControl", "Show or hide the Source Control panel";
    ToggleGitPanel => "workspace::ToggleGitPanel", "Show or hide commit history in the bottom panel";
    ToggleTerminal => "workspace::ToggleTerminal", "Show or hide the terminal panel";
    OpenSettings => "workspace::OpenSettings", "Open the Settings page";
    OpenCommandPalette => "workspace::OpenCommandPalette", "Search and run any command";
    SelectTheme => "workspace::SelectTheme", "Choose a color theme from a list";
    OpenKeymap => "workspace::OpenKeymap", "Open the Keymap page";
    OpenKeymapFile => "workspace::OpenKeymapFile", "Open the user keymap.json";
    OpenExtensions => "workspace::OpenExtensions", "Open the Extensions page";
    ShowExtensions => "workspace::ShowExtensions", "Show the state of the installed extensions";
    ReloadExtensions => "workspace::ReloadExtensions", "Reload the installed extensions from disk";
    ImportExtension => "workspace::ImportExtension", "Install an extension from a .loomext package";
    InspectHighlight => "editor::InspectHighlight", "Show how the code at the cursor is coloured and which query rule decides it";
    ShowQueryProblems => "workspace::ShowQueryProblems", "Show custom highlight queries that were left out, and why";
    RetryExtensions => "workspace::RetryExtensions", "Start the extensions that failed again";
    StopExtensions => "workspace::StopExtensions", "Stop every running extension and its language servers until they are reloaded";
    TrustWorkspace => "workspace::TrustWorkspace", "Trust the current folder so language servers and tools may run";
    RevokeWorkspaceTrust => "workspace::RevokeWorkspaceTrust", "Stop trusting the current folder and stop its servers";
    AllowExtensionTools => "workspace::AllowExtensionTools", "Let extensions run programs from the current folder (no sandbox)";
    RevokeExtensionTools => "workspace::RevokeExtensionTools", "Stop extensions from running programs in the current folder";
    OpenSettingsFile => "workspace::OpenSettingsFile", "Open the user settings.json";
    OpenProjectSettingsFile => "workspace::OpenProjectSettingsFile", "Open the project .loom/settings.json";
    IncreaseFontSize => "workspace::IncreaseFontSize", "Increase the focused editor or terminal font size";
    DecreaseFontSize => "workspace::DecreaseFontSize", "Decrease the focused editor or terminal font size";
    ResetFontSize => "workspace::ResetFontSize", "Reset the focused editor or terminal font size";
    CloneRepository => "workspace::CloneRepository", "Clone a Git repository";
    CloseOverlay => "workspace::CloseOverlay", "Close menus and transient overlays";
    ActivatePaneLeft => "workspace::ActivatePaneLeft", "Focus the pane to the left";
    ActivatePaneRight => "workspace::ActivatePaneRight", "Focus the pane to the right";
    ActivatePaneUp => "workspace::ActivatePaneUp", "Focus the pane above";
    ActivatePaneDown => "workspace::ActivatePaneDown", "Focus the pane below";
    ActivateNextPane => "workspace::ActivateNextPane", "Focus the next pane";
    ActivatePrevPane => "workspace::ActivatePrevPane", "Focus the previous pane";

    ActivateNextItem => "pane::ActivateNextItem", "Activate the next tab";
    ActivatePrevItem => "pane::ActivatePrevItem", "Activate the previous tab";
    ActivateLastItem => "pane::ActivateLastItem", "Activate the last tab";
    CloseActiveItem => "pane::CloseActiveItem", "Close the active tab";
    SplitRight => "pane::SplitRight", "Split the pane, copying the active tab to the right";
    SplitLeft => "pane::SplitLeft", "Split the pane, copying the active tab to the left";
    SplitUp => "pane::SplitUp", "Split the pane, copying the active tab above";
    SplitDown => "pane::SplitDown", "Split the pane, copying the active tab below";
    MoveItemToPaneLeft => "pane::MoveItemToPaneLeft", "Move the active tab to the pane on the left";
    MoveItemToPaneRight => "pane::MoveItemToPaneRight", "Move the active tab to the pane on the right";
    MoveItemToPaneUp => "pane::MoveItemToPaneUp", "Move the active tab to the pane above";
    MoveItemToPaneDown => "pane::MoveItemToPaneDown", "Move the active tab to the pane below";
    ClosePane => "pane::ClosePane", "Close the focused pane and its tabs";

    ToggleMarkdown => "markdown::TogglePreview", "Switch the current Markdown view between source and reading";
    MarkdownPreviewToSide => "markdown::OpenPreviewToSide", "Open or focus the associated Markdown reading view";
    MarkdownOpenSource => "markdown::OpenSource", "Focus the corresponding source view";
    Save => "editor::Save", "Save the active file";
    SaveWithoutExtensions => "editor::SaveWithoutExtensions", "Save the active file without the extensions that change files on save";
    Undo => "editor::Undo", "Undo";
    Redo => "editor::Redo", "Redo";
    SelectAll => "editor::SelectAll", "Select all text";
    Copy => "editor::Copy", "Copy the selection";
    Cut => "editor::Cut", "Cut the selection";
    Paste => "editor::Paste", "Paste from the clipboard";
    Cancel => "editor::Cancel", "Clear the selection or close the editor menu";
    MoveLeft => "editor::MoveLeft", "Move the cursor left";
    MoveRight => "editor::MoveRight", "Move the cursor right";
    MoveUp => "editor::MoveUp", "Move the cursor up";
    MoveDown => "editor::MoveDown", "Move the cursor down";
    SelectLeft => "editor::SelectLeft", "Extend the selection left";
    SelectRight => "editor::SelectRight", "Extend the selection right";
    SelectUp => "editor::SelectUp", "Extend the selection up";
    SelectDown => "editor::SelectDown", "Extend the selection down";
    MoveToPreviousWordStart => "editor::MoveToPreviousWordStart", "Move to the previous word start";
    MoveToNextWordEnd => "editor::MoveToNextWordEnd", "Move to the next word end";
    SelectToPreviousWordStart => "editor::SelectToPreviousWordStart", "Select to the previous word start";
    SelectToNextWordEnd => "editor::SelectToNextWordEnd", "Select to the next word end";
    MoveToBeginningOfLine => "editor::MoveToBeginningOfLine", "Move to the start of the line";
    MoveToEndOfLine => "editor::MoveToEndOfLine", "Move to the end of the line";
    SelectToBeginningOfLine => "editor::SelectToBeginningOfLine", "Select to the start of the line";
    SelectToEndOfLine => "editor::SelectToEndOfLine", "Select to the end of the line";
    MoveToBeginning => "editor::MoveToBeginning", "Move to the start of the file";
    MoveToEnd => "editor::MoveToEnd", "Move to the end of the file";
    SelectToBeginning => "editor::SelectToBeginning", "Select to the start of the file";
    SelectToEnd => "editor::SelectToEnd", "Select to the end of the file";
    MovePageUp => "editor::MovePageUp", "Move up one page";
    MovePageDown => "editor::MovePageDown", "Move down one page";
    SelectPageUp => "editor::SelectPageUp", "Select up one page";
    SelectPageDown => "editor::SelectPageDown", "Select down one page";
    Backspace => "editor::Backspace", "Delete the previous character";
    Delete => "editor::Delete", "Delete the next character";
    DeleteToPreviousWordStart => "editor::DeleteToPreviousWordStart", "Delete to the previous word start";
    DeleteToNextWordEnd => "editor::DeleteToNextWordEnd", "Delete to the next word end";
    Newline => "editor::Newline", "Insert a line break";
    Tab => "editor::Tab", "Indent";
    Fold => "editor::Fold", "Fold code at the caret";
    Unfold => "editor::Unfold", "Unfold code at the caret";
    FoldAll => "editor::FoldAll", "Fold all code regions";
    UnfoldAll => "editor::UnfoldAll", "Unfold all code regions";
    SelectNextOccurrence => "editor::SelectNextOccurrence", "Select the next occurrence";
    AddCursorAbove => "editor::AddCursorAbove", "Add a cursor on the row above";
    AddCursorBelow => "editor::AddCursorBelow", "Add a cursor on the row below";
    ToggleSoftWrap => "editor::ToggleSoftWrap", "Toggle soft wrapping in the current document";
    MoveLineUp => "editor::MoveLineUp", "Move selected lines up";
    MoveLineDown => "editor::MoveLineDown", "Move selected lines down";
    DuplicateLineUp => "editor::DuplicateLineUp", "Duplicate selected lines above";
    DuplicateLineDown => "editor::DuplicateLineDown", "Duplicate selected lines below";
    DeleteLine => "editor::DeleteLine", "Delete selected lines";
    ToggleComment => "editor::ToggleComment", "Toggle line comments for the current language";
    Backtab => "editor::Backtab", "Outdent";
    ShowCompletions => "editor::ShowCompletions", "Show the completion list";
    NextSnippetTabstop => "editor::NextSnippetTabstop", "Move to the next place to fill in a snippet";
    PreviousSnippetTabstop => "editor::PreviousSnippetTabstop", "Move to the previous place to fill in a snippet";
    ShowSnippetChoices => "editor::ShowSnippetChoices", "Choose a value for the current snippet placeholder";
    ShowSignatureHelp => "editor::ShowSignatureHelp", "Show the parameters of the call being typed";
    NextSignature => "editor::NextSignature", "Show the next signature of the call";
    PreviousSignature => "editor::PreviousSignature", "Show the previous signature of the call";
    SelectLargerSyntaxNode => "editor::SelectLargerSyntaxNode", "Grow the selection to the enclosing syntax";
    SelectSmallerSyntaxNode => "editor::SelectSmallerSyntaxNode", "Shrink the selection back";
    Rename => "editor::Rename", "Rename the symbol at the cursor";
    ToggleCodeActions => "editor::ToggleCodeActions", "Show the fixes and refactorings offered here";
    Format => "editor::Format", "Format the file";
    FormatSelections => "editor::FormatSelections", "Format the selection";
    ShowCallHierarchy => "editor::ShowCallHierarchy", "List the callers of the function at the cursor";
    ShowOutgoingCalls => "editor::ShowOutgoingCalls", "List what the function at the cursor calls";
    ShowSupertypes => "editor::ShowSupertypes", "List the supertypes of the type at the cursor";
    ShowSubtypes => "editor::ShowSubtypes", "List the subtypes of the type at the cursor";
    ShowCodeLenses => "editor::ShowCodeLenses", "List the code lenses of the file";
    ToggleProblems => "workspace::ToggleProblems", "List the problems of the open files";
    GoToDefinition => "editor::GoToDefinition", "Go to the definition of the symbol at the cursor";
    PeekDefinition => "editor::PeekDefinition", "Preview the definition without leaving this file";
    OpenPeekTarget => "editor::OpenPeekTarget", "Open the previewed definition";
    ClosePeek => "editor::ClosePeek", "Close the definition preview";
    GoToDeclaration => "editor::GoToDeclaration", "Go to the declaration of the symbol at the cursor";
    GoToTypeDefinition => "editor::GoToTypeDefinition", "Go to the type of the symbol at the cursor";
    GoToImplementation => "editor::GoToImplementation", "Go to the implementations of the symbol at the cursor";
    FindAllReferences => "editor::FindAllReferences", "List the references to the symbol at the cursor";
    ShowHover => "editor::Hover", "Show what the language servers say about the symbol at the cursor";
    GoToDiagnostic => "editor::GoToDiagnostic", "Go to the next problem in the file";
    GoToPreviousDiagnostic => "editor::GoToPreviousDiagnostic", "Go to the previous problem in the file";
    GoToConflict => "editor::GoToConflict", "Go to the next merge conflict in the file";
    GoToPreviousConflict => "editor::GoToPreviousConflict", "Go to the previous merge conflict in the file";
    GoBack => "pane::GoBack", "Go back to the previous place";
    GoForward => "pane::GoForward", "Go forward to the next place";
    ToggleOutline => "outline::Toggle", "Go to a symbol in this file";
    ToggleProjectSymbols => "project_symbols::Toggle", "Go to a symbol in the workspace";

    TerminalCopy => "terminal::Copy", "Copy the terminal selection";
    TerminalCopyOrInterrupt => "terminal::CopyOrInterrupt", "Copy the selection, or send Ctrl+C without one";
    TerminalPaste => "terminal::Paste", "Paste into the terminal";

    MenuCancel => "menu::Cancel", "Close the open menu";
    MenuSelectNext => "menu::SelectNext", "Select the next menu item";
    MenuSelectPrevious => "menu::SelectPrevious", "Select the previous menu item";
    MenuConfirm => "menu::Confirm", "Choose the selected menu item";
    MenuConfirmTab => "menu::ConfirmWithTab", "Choose the selected menu item with the Tab key";
    MenuPageUp => "menu::PageUp", "Move the selection up one page";
    MenuPageDown => "menu::PageDown", "Move the selection down one page";

    GitCommit => "git::Commit", "Commit the staged changes";

    DialogConfirm => "dialog::Confirm", "Confirm the open dialog";
    DialogCancel => "dialog::Cancel", "Dismiss the open dialog";
    DialogSecondary => "dialog::Secondary", "Choose the dialog's secondary option";
}

/// Command id of an `extension::Run` binding, interned so `Action` stays `Copy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct ExtensionCommand(usize);

fn interned_commands() -> &'static std::sync::Mutex<Vec<String>> {
    static COMMANDS: std::sync::OnceLock<std::sync::Mutex<Vec<String>>> =
        std::sync::OnceLock::new();
    COMMANDS.get_or_init(Default::default)
}

impl ExtensionCommand {
    pub fn intern(id: &str) -> Self {
        let mut interned = interned_commands().lock().expect("interned commands lock");
        let index = interned
            .iter()
            .position(|known| known == id)
            .unwrap_or_else(|| {
                interned.push(id.to_owned());
                interned.len() - 1
            });
        Self(index)
    }

    pub fn id(self) -> String {
        interned_commands().lock().expect("interned commands lock")[self.0].clone()
    }
}

/// Keys of a `vim::Keys` binding, interned so `Action` stays `Copy`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct VimKeys(usize);

fn interned_keys() -> &'static std::sync::Mutex<Vec<String>> {
    static KEYS: std::sync::OnceLock<std::sync::Mutex<Vec<String>>> = std::sync::OnceLock::new();
    KEYS.get_or_init(Default::default)
}

impl VimKeys {
    pub fn intern(keys: &str) -> Self {
        let mut interned = interned_keys().lock().expect("interned keys lock");
        let index = interned
            .iter()
            .position(|known| known == keys)
            .unwrap_or_else(|| {
                interned.push(keys.to_owned());
                interned.len() - 1
            });
        Self(index)
    }

    pub fn keys(self) -> String {
        interned_keys().lock().expect("interned keys lock")[self.0].clone()
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ActionError(pub String);

impl Action {
    /// Parse a keymap value: `"name"`, `["name", argument]` or `null`.
    pub fn from_json(value: &Value) -> Result<Self, ActionError> {
        match value {
            Value::Null => Ok(Action::NoAction),
            Value::String(name) => {
                if ACTIONS
                    .iter()
                    .any(|meta| meta.name == name && meta.takes_argument)
                {
                    return Err(ActionError(format!(
                        "`{name}` needs an argument: [\"{name}\", …]"
                    )));
                }
                unit_action(name).ok_or_else(|| ActionError(format!("unknown action `{name}`")))
            }
            Value::Array(items) => {
                let [Value::String(name), argument] = items.as_slice() else {
                    return Err(ActionError(
                        "expected [\"action::Name\", argument]".to_owned(),
                    ));
                };
                match name.as_str() {
                    "pane::ActivateItem" => argument
                        .as_u64()
                        .map(|index| Action::ActivateItem(index as usize))
                        .ok_or_else(|| {
                            ActionError(
                                "`pane::ActivateItem` needs a non-negative index".to_owned(),
                            )
                        }),
                    "extension::Run" => argument
                        .as_str()
                        .filter(|id| {
                            id.split('.').count() >= 3 && !id.contains(char::is_whitespace)
                        })
                        .map(|id| Action::RunExtension(ExtensionCommand::intern(id)))
                        .ok_or_else(|| {
                            ActionError(
                                "`extension::Run` needs a command id like \"vendor.name.command\""
                                    .to_owned(),
                            )
                        }),
                    "vim::Keys" => argument
                        .as_str()
                        .filter(|keys| !keys.is_empty())
                        .map(|keys| Action::VimKeys(VimKeys::intern(keys)))
                        .ok_or_else(|| {
                            ActionError(
                                "`vim::Keys` needs keys in Vim notation, e.g. \"<Esc>\"".to_owned(),
                            )
                        }),
                    _ if unit_action(name).is_some() => {
                        Err(ActionError(format!("`{name}` does not take an argument")))
                    }
                    _ => Err(ActionError(format!("unknown action `{name}`"))),
                }
            }
            _ => Err(ActionError(
                "expected an action name, [name, argument] or null".to_owned(),
            )),
        }
    }

    pub fn namespace(self) -> &'static str {
        self.name().split("::").next().unwrap_or_default()
    }

    /// The editor primitive an `editor::` action performs. `page` is the
    /// number of visible lines, used by page movements.
    pub fn editor_command(self, page: usize) -> Option<Command> {
        use Movement::*;
        Some(match self {
            Action::Undo => Command::Undo,
            Action::Redo => Command::Redo,
            Action::SelectAll => Command::SelectAll,
            Action::Copy => Command::Copy,
            Action::Cut => Command::Cut,
            Action::Paste => Command::Paste,
            Action::Cancel => Command::Escape,
            Action::MoveLeft => Command::Move(Left, false),
            Action::MoveRight => Command::Move(Right, false),
            Action::MoveUp => Command::Move(Up, false),
            Action::MoveDown => Command::Move(Down, false),
            Action::SelectLeft => Command::Move(Left, true),
            Action::SelectRight => Command::Move(Right, true),
            Action::SelectUp => Command::Move(Up, true),
            Action::SelectDown => Command::Move(Down, true),
            Action::MoveToPreviousWordStart => Command::Move(WordLeft, false),
            Action::MoveToNextWordEnd => Command::Move(WordRight, false),
            Action::SelectToPreviousWordStart => Command::Move(WordLeft, true),
            Action::SelectToNextWordEnd => Command::Move(WordRight, true),
            Action::MoveToBeginningOfLine => Command::Move(Home, false),
            Action::MoveToEndOfLine => Command::Move(End, false),
            Action::SelectToBeginningOfLine => Command::Move(Home, true),
            Action::SelectToEndOfLine => Command::Move(End, true),
            Action::MoveToBeginning => Command::Move(Start, false),
            Action::MoveToEnd => Command::Move(Finish, false),
            Action::SelectToBeginning => Command::Move(Start, true),
            Action::SelectToEnd => Command::Move(Finish, true),
            Action::MovePageUp => Command::Move(PageUp(page), false),
            Action::MovePageDown => Command::Move(PageDown(page), false),
            Action::SelectPageUp => Command::Move(PageUp(page), true),
            Action::SelectPageDown => Command::Move(PageDown(page), true),
            Action::Backspace => Command::Delete(false, false),
            Action::Delete => Command::Delete(true, false),
            Action::DeleteToPreviousWordStart => Command::Delete(false, true),
            Action::DeleteToNextWordEnd => Command::Delete(true, true),
            Action::Newline => Command::Enter,
            Action::Tab => Command::Indent(false),
            Action::MoveLineUp => Command::Line(loom_editor_core::normal::LineOperation::MoveUp),
            Action::MoveLineDown => {
                Command::Line(loom_editor_core::normal::LineOperation::MoveDown)
            }
            Action::DuplicateLineUp => {
                Command::Line(loom_editor_core::normal::LineOperation::CopyUp)
            }
            Action::DuplicateLineDown => {
                Command::Line(loom_editor_core::normal::LineOperation::CopyDown)
            }
            Action::DeleteLine => Command::Line(loom_editor_core::normal::LineOperation::Delete),
            Action::ToggleComment => Command::ToggleComment,
            Action::Backtab => Command::Indent(true),
            _ => return None,
        })
    }

    /// The `editor::` action that performs a context-menu command, so menu
    /// labels can show its binding.
    pub fn for_editor_command(command: Command) -> Option<Self> {
        Some(match command {
            Command::Undo => Action::Undo,
            Command::Redo => Action::Redo,
            Command::SelectAll => Action::SelectAll,
            Command::Copy => Action::Copy,
            Command::Cut => Action::Cut,
            Command::Paste => Action::Paste,
            _ => return None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn names_round_trip_for_every_listed_action() {
        for meta in ACTIONS {
            let value = if meta.name == "vim::Keys" {
                json!([meta.name, "<Esc>"])
            } else if meta.name == "extension::Run" {
                json!([meta.name, "example.tools.uppercase"])
            } else if meta.takes_argument {
                json!([meta.name, 0])
            } else {
                json!(meta.name)
            };
            let action = Action::from_json(&value).unwrap();
            assert_eq!(action.name(), meta.name);
        }
    }

    #[test]
    fn parses_arguments_null_and_reports_mistakes() {
        assert_eq!(
            Action::from_json(&json!(["pane::ActivateItem", 3])),
            Ok(Action::ActivateItem(3))
        );
        assert_eq!(Action::from_json(&Value::Null), Ok(Action::NoAction));
        assert!(Action::from_json(&json!("pane::ActivateItem")).is_err());
        assert!(Action::from_json(&json!(["pane::ActivateItem", -1])).is_err());
        assert!(Action::from_json(&json!(["editor::Undo", 1])).is_err());
        assert!(Action::from_json(&json!("editor::Nope")).is_err());
        assert!(Action::from_json(&json!(42)).is_err());
    }

    #[test]
    fn extension_commands_are_a_typed_reference_with_an_argument() {
        let action =
            Action::from_json(&json!(["extension::Run", "example.tools.uppercase"])).unwrap();
        let Action::RunExtension(command) = action else {
            panic!("{action:?}")
        };
        assert_eq!(command.id(), "example.tools.uppercase");
        assert_eq!(action.name(), "extension::Run");
        assert_eq!(
            action,
            Action::from_json(&json!(["extension::Run", "example.tools.uppercase"])).unwrap()
        );
        // A bare name, a malformed id and a non-string are all refused.
        assert!(Action::from_json(&json!("extension::Run")).is_err());
        assert!(Action::from_json(&json!(["extension::Run", "uppercase"])).is_err());
        assert!(Action::from_json(&json!(["extension::Run", 3])).is_err());
    }

    #[test]
    fn editor_actions_map_to_commands() {
        assert_eq!(
            Action::SelectPageDown.editor_command(20),
            Some(Command::Move(Movement::PageDown(20), true))
        );
        assert_eq!(
            Action::Backtab.editor_command(1),
            Some(Command::Indent(true))
        );
        assert_eq!(Action::Save.editor_command(1), None);
        assert_eq!(Action::Save.namespace(), "editor");
    }
}
