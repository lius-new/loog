//! Extension model: manifest, activation, command precedence, lifecycle and
//! trust rules. No GUI, file system, network or Wasm runtime lives here; the
//! runtime is in `infrastructure/extensions` and orchestration in
//! `app/extensions`.
pub mod activation;
pub mod command;
pub mod lifecycle;
pub mod manifest;
pub mod servers;
pub mod settings;
pub mod trust;
