//! Application composition of the pure layout domain.
#[derive(Clone, Default)]
pub struct ApplicationLayoutState {
    pub application_layout: Option<crate::domain::application_layout::ApplicationLayout>,
    /// Opening remembers the content focus to restore when the popup closes.
    pub visibility_popup: Option<bool>,
    pub application_sash_drag: Option<crate::domain::application_layout::Divider>,
}
