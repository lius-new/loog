# Independent workspace-domain verification

```sh
cargo test --locked --offline -p loog-workspace-tests
```

This verification package compiles the actual `src/domain` sources with only
the editing core, serde and serde_json. No application, GUI, disk adapter,
terminal or Git implementation is linked. It is not a production dependency.
The `isolated` test namespace distinguishes its tests in workspace reports.

The application also runs these rules through its existing event and file/session
integration tests. Including a verification entry point does not create a second
runtime state container or a duplicate implementation.
