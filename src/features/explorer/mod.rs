//! Explorer feature: directory listings, workspace roots and naming rules.
//! No dependency on the application, GUI or filesystem; disk access is
//! injected through `ports`.
pub mod edit;
pub mod model;
pub mod ports;
pub mod roots;
mod store;
pub mod tree;
pub mod types;
pub use self::tree::ListingRef;
pub use self::types::*;

#[cfg(test)]
pub(crate) mod tests;
