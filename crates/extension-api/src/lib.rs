//! Value types and pure rules shared by the extension host and SDKs.
//!
//! Nothing here touches a GUI, a Wasm runtime or the file system. The host
//! (`src/app/extensions`, `src/infrastructure/extensions`) and the SDKs
//! consume these rules so that both sides agree on identities, errors,
//! edit validation, admission limits, receipts and call budgets.
pub mod admission;
pub mod budget;
pub mod edit;
pub mod error;
pub mod events;
pub mod ledger;
pub mod scope;
pub mod utf16;

pub use self::error::{ErrorKind, ExtensionError};
