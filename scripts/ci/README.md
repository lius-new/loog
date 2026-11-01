# Lean CI and additional validation

The everyday CI path contains formatting/architecture checks, independent crate
tests once on Linux, and all host functional tests on Linux, Windows and macOS.
Clippy runs in the Linux host job and reuses its native setup. There are no npm
installs, tool self-tests, WASM installation tests, source-removal rebuilds,
evidence uploads or release builds in the ordinary path.

Only main pushes, pull requests and manual dispatch trigger CI. Feature-branch
pushes are covered by their PR. Main pushes do not force deep integration.
Preview packaging runs for relevant PRs or explicit full/manual validation;
main pushes leave final package construction to the single Release workflow.

## Additional validation

validation.yml is a reusable workflow called only when changed paths select work.
It can also be dispatched manually; its default full=true runs every group.
CI's own manual full input defaults to false.

| Group | Selected changes |
| --- | --- |
| tooling | Script/test tooling and staged resources; self-tests once per OS |
| evidence | Markdown, shared editor/domain, typography/settings and assets |
| validator | Production language/manifest rules, validator sources and extension fixtures; CLI tests once per OS |
| standalone | Language-source independence and host entry point |
| package | PR packaging/staging changes and packaged resources |

CI/toolchain/dependency/vendor policy changes, unknown paths or an unavailable
Git range select the conservative deep checks. Main still omits preview release
packaging. PRs use merge-base-to-head diffs; pushes use before-to-after diffs.
NUL separation and disabled rename folding retain deleted and moved paths.

Dependency setup only restores caches and verifies/exports required inputs.
It never runs self-tests. Cargo registry/Git sources and hash-verified tool
downloads are cached; credentials and installed system tools are not cached.
Release compile prepares the required Skia target once (two slices for macOS).

CI result always aggregates mandatory and selected jobs. A selected validation
that failed, was cancelled or skipped cannot pass the gate. Use CI result as the
stable branch-protection check; repository settings are not changed here.

## Validation commands

~~~sh
npm ci --ignore-scripts --prefix scripts/ci
node scripts/ci/test-tools.mjs
node scripts/architecture/check.mjs
cargo test --locked --manifest-path tools/ext-validate/Cargo.toml --test cli
~~~

The suites cover real Git merge bases/renames/deletions, optional scope and result
aggregation, YAML contracts, exact-commit release prerequisites, publication
ordering and local artifact integrity. Linux/macOS runner execution must still
be validated in GitHub Actions; local tests never create a remote release.

Release uses tag-driven versions. Native CI tests the original source SHA;
release runners generate only the two Cargo version fields in their working
copies before compiling. Preview packaging reads its baseline Cargo version
without maintaining a separate workflow version.
