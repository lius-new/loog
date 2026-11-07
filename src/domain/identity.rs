//! One shared identity definition for the core and application models.
pub use loog_editor_core::identity::{FileId, PaneId, ViewId};

/// An immutable comparison, outside the editable document table.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct DiffId(u64);
impl DiffId {
    pub const fn new(value: u64) -> Self {
        Self(value)
    }
    pub const fn get(self) -> u64 {
        self.0
    }
}
