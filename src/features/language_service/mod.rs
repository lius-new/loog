//! The language server protocol client's rules, without any I/O: framing, JSON-RPC
//! messages, capability negotiation and the session state machine. Starting
//! processes and moving bytes belongs to `infrastructure::lsp`.
pub(crate) mod capabilities;
pub(crate) mod completion;
pub(crate) mod diagnostics;
pub(crate) mod dynamic;
pub(crate) mod framing;
pub(crate) mod fuzzy;
pub(crate) mod message;
pub(crate) mod plan;
pub(crate) mod position;
pub(crate) mod providers;
pub(crate) mod reply;
pub(crate) mod request;
pub(crate) mod root;
pub(crate) mod semantic;
pub(crate) mod server_set;
pub(crate) mod session;
#[cfg(test)]
mod session_tests;
pub(crate) mod snippet;
pub(crate) mod sync;
pub(crate) mod uri;
pub(crate) mod watch;
