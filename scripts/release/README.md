# Release notes and drafts

Enable local commit checks with `node scripts/commit/install.mjs`. Every new
commit must have exactly one `Release Notes:` section, including `- N/A` for
internal changes. CI also validates the complete introduced commit range; see
[CONTRIBUTING.md](../../CONTRIBUTING.md).

Write English release notes in the body of the commit that introduces a change.
For pull requests, write the same section in the PR description and preserve it
in the squash commit message. A conventional commit title is optional; the notes
generator reads the body, not the title.

```text
feat(editor): support selecting documentation text

Release Notes:

- Added text selection and copying in editor documentation cards.
- Improved wrapping of long diagnostic messages.
- Fixed completion labels drawing matched characters twice.
- Upgrade: Replace `ask` and `never` installation settings with `manual`.
```

Use `- N/A` for internal work with no user-facing changes. Use the exact
`Release Notes:` heading and top-level bullets starting with `Added`, `Improved`,
`Changed`, `Removed`, `Fixed`, `Upgrade:` or `Breaking change:`. Indent wrapped
continuation lines. Put Git trailers such as `Co-authored-by:` after the section. Write prose without
icons. Upgrade entries must describe the action users need to take.

Preview the next version without creating a tag or contacting GitHub:

```sh
node scripts/release/notes.mjs --version 0.1.3 --output target/release-notes.md
node --test scripts/release/release.test.mjs
```

The generator reads the complete commit bodies between the nearest reachable
application version tag and the selected commit. Tags for language packages and
language servers are excluded. It also works when the current application tag
does not exist yet. `- N/A` entries are omitted, identical entries are deduplicated,
and recorded changes are grouped under Added, Improved, Changed, Removed, Fixed
and Upgrade notes, with commit links and a full changelog link. Commits without
a section are listed as warnings and omitted; their titles are not guessed into
user-facing prose.
Malformed sections fail generation instead of silently dropping their contents.

## Publishing

1. Commit product changes and wait for their exact-SHA CI to succeed. There is
   no per-release source version bump. The tag is the release version authority.
   The bundled extension lock is not part of the commit: the Release workflow's
   verify job generates it once from the extensions registry's live signed index
   (`node scripts/package/refresh-lock.mjs`, which checks the signature against
   the application's trusted keys) and hands it to every platform build. It is kept
   as the `registry-lock` run artifact, which names the exact index a release bundled.
2. Create and push a version tag such as v0.2.0 on that tested commit. Release accepts successful push/manual CI runs from this
   repository, never a PR run that tested a synthetic merge commit. The latest
   matching run and all three host checks plus CI result must have succeeded.
3. The single Release workflow validates the tag/version/CI before building.
   Each platform verifies its checkout SHA and temporarily materializes the tag
   version into Cargo.toml and the root Loom entry in Cargo.lock. Only these two
   version fields may change. HEAD/tags/index remain unchanged; dependencies stay
   locked. Each platform then runs setup, compile, package and bundle once. It does not rerun
   workspace tests, extension-validator tests, development-tool tests or evidence capture.
4. Final product checks remain: staged Windows startup/normal close, Linux .deb
   installation, macOS .dmg/signature checks, and signed update manifest creation.
5. Once all platform jobs succeed, one writer downloads this run's artifacts,
   checks every platform package/checksum/update record, creates or reuses the
   draft and uploads the complete set. It never changes existing draft notes,
   creates/moves tags, or publishes the release publicly.
6. Review all packages and notes, then publish the draft manually on GitHub.

Manual Release dispatch defaults to publish=false and still requires exact-SHA
CI. Enable publish to upload the resulting packages to a draft. Missing, pending
or failed CI stops before builds; finish/rerun CI and rerun Release. There is no
cross-workflow polling or background CI rerun.

Each application release also includes `loom-ext-validate` for Windows x64,
Linux x64 and macOS arm64. It uses the application's production manifest rules,
language loader and query compilers; see [the validator](../../tools/ext-validate/README.md).
After the release is published, pin its validator URLs and SHA-256 digests in
`tools/validator.lock` in the extensions repository. Do not pin a local build or
an asset that has not been published. The extensions repository can use a local
build through `LOOM_EXT_VALIDATE` until the first validator release is available.

Transient GitHub transport/429/5xx errors retry only the current operation up to
three attempts with bounded subprocess timeouts. Missing drafts and permission
errors are not polled. Failed publishing can be rerun without rebuilding the
successful platform jobs, while their artifacts remain available. Reruns must
retain the same verified tag/commit. A published release is left untouched.

Portable preview packaging, evidence, tool self-tests and source-removal/WASM
verification remain available through Additional validation. Production build
dependencies and checksums stay verified even when tool tests are not selected.

The generated Cargo version feeds CARGO_PKG_VERSION, Windows resources, package
names and signed update manifests. Cargo.toml may retain its development
baseline version in Git. Manual releases require an explicit existing tag
version and have no hardcoded version default. Preview packages read the
development version from Cargo.toml automatically. Nothing commits or pushes
generated version changes. The workflow implementation itself must first be
committed normally before GitHub can execute it.
