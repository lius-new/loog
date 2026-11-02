//! Native language-feature presentation models. No protocol client or IO.
pub(crate) mod capabilities {
    pub use crate::features::providers::PositionEncoding;
}
pub(crate) mod completion;
pub(crate) mod diagnostics;
pub(crate) mod fuzzy;
pub(crate) use crate::features::providers::position;
pub(crate) mod providers;
pub(crate) mod reply;
pub(crate) mod request {
    pub use crate::features::providers::{Feature, Merge};
}
pub(crate) mod semantic;
pub(crate) mod snippet;
pub(crate) mod sync {
    pub use crate::features::providers::Stamp;
}
pub(crate) mod uri;
