//! Transport-independent language capability consumption.
mod feature;
mod request;
pub use feature::{Feature, Merge};
pub(crate) mod position;
pub use position::PositionEncoding;
mod results;
pub(crate) use request::{
    Cancellation, EXTERNAL_RANK, EXTERNAL_SOURCE, Failure, Part, PartSink, RequestHandle,
    RequestSpec,
};
pub(crate) use results::{CodeActionContext, DocumentResults, ResultStore};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Stamp {
    pub incarnation: u64,
    pub version: u64,
}
pub(crate) const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);
#[derive(Clone, Debug)]
pub(crate) enum FileChange {
    Created(std::path::PathBuf),
    Renamed {
        from: std::path::PathBuf,
        to: std::path::PathBuf,
    },
    Deleted(std::path::PathBuf),
}
