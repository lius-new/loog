//! Loog — a modular text editor port of the "Aura Code" mockup.
//!
//! Layer map:
//!   app      — root component, layout & composition
//!   app/state — legacy reactive model; field ownership is registered
//!   theme    — design tokens (colors, metrics, text styles)
//!   domain/  — framework-free types; model/ remains a tracked legacy layer
//!   editor/  — code surface (syntax, gutter, viewport)
//!   ui/      — chrome components (titlebar, sidebar, tabs, …)
//!   features/keymap — raw events → semantic actions (input alias during migration)
// Keep debug builds attached to the launching console so `cargo run` can be
// stopped with Ctrl+C. Test binaries also need a console for native PTY probes.
// Packaged release builds remain windowed and do not open a console window.
#![cfg_attr(
    all(target_os = "windows", not(debug_assertions), not(test)),
    windows_subsystem = "windows"
)]
// The MSVC linker reports the export library it makes because the bundled
// Tree-sitter C code exports its API; that line is not a problem.
#![allow(linker_messages)]
#![allow(dead_code)] // the data layer & palette expose an intentionally broad API

mod app;
mod domain;
mod features;
mod infrastructure;
mod model;
mod ui;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    app::run()
}
