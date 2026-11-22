//! Content search feature. No dependency on the application or GUI.
pub mod matcher;
pub mod model;
pub mod ports;
pub mod service;
pub mod stream;
pub mod types;
pub use self::matcher::{HASH_SEED, Matcher, hash, hash_continue};
pub use self::service::{MAX_FILE_BYTES, MAX_MATCHES};
pub use self::types::*;

pub mod navigation;

#[cfg(test)]
pub(crate) mod tests;
