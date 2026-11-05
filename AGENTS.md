# Repository instructions

Keep the application/plugin boundary in
[shared extension UI](docs/architecture/shared-extension-ui.md): application
capabilities must be generic. Never add plugin IDs, shared keys, server names,
status text or plugin-specific containers/policies to production application code.

Prefer focused checks for the changed code. Do not run the full regression suite
on each iteration; explain why broader coverage is necessary before expanding it.

Every commit must contain exactly one `Release Notes:` section in its body.
Use English release-note bullets for user-facing changes. For internal changes,
include this explicitly:

```text
Release Notes:
- N/A
```

Do not automatically classify user-facing changes as N/A. Follow
[scripts/release/README.md](scripts/release/README.md) for accepted note types.
Enable local enforcement with `node scripts/commit/install.mjs`; CI validates
new commits too. Preserve this section when amending, squashing or merging.

Follow the [compatibility code policy](CONTRIBUTING.md#compatibility-code).
Every new or modified compatibility branch or adapter needs a local
`COMPAT(<stable-reason>)` comment covering the supported legacy behavior,
retention reason, replacement, and removal condition. Explicitly state when
no removal version is scheduled; do not invent a deprecation plan. Reassess
existing compatibility paths when touching them, and update compatibility-only
tests, documentation, and markers when removing them. Apply this during code
reviews as well as implementation.
