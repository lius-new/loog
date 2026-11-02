//! Generic provider options used by native editor interactions.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompletionOptions {
    pub trigger_characters: Vec<String>,
    pub resolve: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SignatureOptions {
    pub trigger_characters: Vec<String>,
    pub retrigger_characters: Vec<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CodeActionOptions {
    pub resolve: bool,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct OnTypeFormatting {
    pub first: String,
    pub more: Vec<String>,
}
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Providers {
    pub hover: bool,
    pub definition: bool,
    pub declaration: bool,
    pub type_definition: bool,
    pub implementation: bool,
    pub references: bool,
    pub document_highlight: bool,
    pub document_symbol: bool,
    pub workspace_symbol: bool,
    pub completion: Option<CompletionOptions>,
    pub signature_help: Option<SignatureOptions>,
    pub rename: Option<bool>,
    pub code_action: Option<CodeActionOptions>,
    pub formatting: bool,
    pub range_formatting: bool,
    pub on_type_formatting: Option<OnTypeFormatting>,
    pub inlay_hint: bool,
    pub folding_range: bool,
    pub selection_range: bool,
    pub call_hierarchy: bool,
    pub type_hierarchy: bool,
    pub code_lens: bool,
    pub execute_command: Vec<String>,
    pub pull_diagnostics: bool,
}
