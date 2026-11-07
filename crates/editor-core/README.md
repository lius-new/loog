# Loog editing core

This crate owns text, selections, exact edit/history records, ordinary editing
behaviour, semantic commands, Vim behaviour and opaque file/pane identities.
It depends only on regex and Unicode libraries. It has no lgui, Skia, filesystem,
window, clipboard implementation or PTY dependency.

```sh
cargo test --locked --offline -p loog-editor-core
```

That command selects this package without compiling Loog's binary, GUI or build
script. Application regression tests still run with:

```sh
cargo test --locked --workspace
```

`EditorMut` borrows one document and its active/peer selections for the whole
edit. The host keeps document sharing and multi-pane transactions together;
the extraction does not create separate mutable buffers per view.

The host converts platform keyboard events into `commands::Command` and
`vim::Key`. `commands::apply` handles text/selection/history commands; Copy,
Cut and Paste are intercepted by the host for clipboard access, as in Loog's
existing editor adapter. Vim asks its injected `Host`/`Clipboard` for capabilities
and emits Save/Quit/Scroll requests instead of operating on files or windows.

`diagnostics` enables a read-only undo/redo byte measurement used by application
performance tests. It is enabled through Loog's dev-dependency, not by default
in the release application.

The application currently retains compatibility exports at `model::text`,
`model::document::FileId`, `model::pane_layout::PaneId` and
`crate::vim`. These refer to the same core definitions; there is no second copy
of the algorithms or identity types.

Application transactions and Vim host orchestration are in `src/app/editor.rs`
and `src/app/editor_vim.rs`. `src/app/editor_view.rs` supplies pane snapshots
and event callbacks to `src/ui/editor`; the UI receives cursor/highlight data
instead of a mutable Workspace or the Vim state machine.
