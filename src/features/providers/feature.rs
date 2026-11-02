//! Editor capabilities and their aggregation policy, independent of transport.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Feature {
    Hover,
    Definition,
    Declaration,
    TypeDefinition,
    Implementation,
    References,
    DocumentHighlight,
    DocumentSymbol,
    WorkspaceSymbol,
    Completion,
    CompletionResolve,
    SignatureHelp,
    PrepareRename,
    Rename,
    CodeAction,
    CodeActionResolve,
    Formatting,
    RangeFormatting,
    OnTypeFormatting,
    InlayHint,
    FoldingRange,
    SelectionRange,
    PrepareCallHierarchy,
    IncomingCalls,
    OutgoingCalls,
    PrepareTypeHierarchy,
    Supertypes,
    Subtypes,
    CodeLens,
    CodeLensResolve,
    ExecuteCommand,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Merge {
    All,
    First,
    Primary,
    Target,
}
impl Feature {
    pub fn merge(self) -> Merge {
        match self {
            Self::Hover
            | Self::Definition
            | Self::Declaration
            | Self::TypeDefinition
            | Self::Implementation
            | Self::References
            | Self::WorkspaceSymbol
            | Self::Completion
            | Self::CodeAction
            | Self::InlayHint
            | Self::CodeLens => Merge::All,
            Self::DocumentHighlight
            | Self::DocumentSymbol
            | Self::SignatureHelp
            | Self::PrepareRename
            | Self::Rename
            | Self::FoldingRange
            | Self::SelectionRange
            | Self::PrepareCallHierarchy
            | Self::PrepareTypeHierarchy => Merge::First,
            Self::Formatting | Self::RangeFormatting | Self::OnTypeFormatting => Merge::Primary,
            Self::CompletionResolve
            | Self::CodeActionResolve
            | Self::CodeLensResolve
            | Self::IncomingCalls
            | Self::OutgoingCalls
            | Self::Supertypes
            | Self::Subtypes
            | Self::ExecuteCommand => Merge::Target,
        }
    }
    pub fn about_document(self) -> bool {
        !matches!(
            self,
            Self::WorkspaceSymbol
                | Self::CompletionResolve
                | Self::CodeActionResolve
                | Self::CodeLensResolve
                | Self::IncomingCalls
                | Self::OutgoingCalls
                | Self::Supertypes
                | Self::Subtypes
                | Self::ExecuteCommand
        )
    }
    pub fn replaces_older(self) -> bool {
        matches!(
            self,
            Self::Hover
                | Self::Completion
                | Self::SignatureHelp
                | Self::DocumentHighlight
                | Self::CodeAction
                | Self::InlayHint
                | Self::FoldingRange
                | Self::CodeLens
                | Self::DocumentSymbol
                | Self::PrepareRename
        )
    }
}
