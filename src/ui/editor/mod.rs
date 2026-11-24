//! Text rendering and geometry; application transactions are supplied through a binding.
mod caret;
pub(crate) mod docs;
mod gutter;
pub(crate) mod hints;
mod indent;
pub(crate) mod keys;
mod lightbulb;
pub(crate) mod menu;
pub(crate) mod peek;
pub(crate) mod popup;
pub(crate) mod signature;
mod syntax;
pub(crate) mod view;

pub(crate) mod projection;

mod fold;
