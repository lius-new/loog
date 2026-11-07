# Updates

Packaged builds of Loog check for a new release about ten seconds after
startup and then once a day. When one is available, a badge with a download
icon and the new version (**⭳ x.y.z**) appears in the title bar, left of
Settings:

| Install | Clicking the badge downloads | Then **Restart to update** |
| --- | --- | --- |
| Windows installer (`…-setup.exe`) | the new setup | runs it silently; it keeps the install folder and the options chosen at install time and closes any other Loog windows |
| macOS `Loog.app` (from the .dmg or .zip) | the new `.zip` | replaces `Loog.app` where it is |
| Linux AppImage | the new AppImage | replaces the AppImage file |
| Linux `.tar.gz` (run in place or through `install.sh`) | the new `.tar.gz` | replaces the install folder, for example `~/.local/lib/loog` |
| Windows portable, Linux `.deb`, read-only installs | nothing | — the badge opens the release page instead |

While the download runs, the badge shows the percentage with a progress line.
Each download is checked against the size and SHA-256 in the manifest before
it is used.

**Restart to update** closes Loog the usual way: unsaved files are offered
for saving first, and the session is restored afterwards. Loog is then
replaced and started again. Running terminal programs end with the restart.
If the restart is cancelled, the update is applied the next time Loog quits.

On macOS and Linux, a small `sh` script waits for Loog to exit, unpacks or
copies the new version next to the old one and swaps them with a rename, so
a failed update leaves the old version in place. Its output goes to
`apply.log` in the `loog-update` folder of the temporary directory. Loog
updates in place only when it can write next to its install: a `Loog.app`
opened straight from Downloads (macOS runs it from a hidden read-only copy
until it is moved to Applications), or a folder owned by another user, gets
the release page instead. The `.deb` is left to the package manager.

Turn checks off with Settings → Updates → **Check for updates**, or
`"check_for_updates": false` in `settings.json`. Builds made with
`cargo build` never check.

Settings → About → **Version** shows the running version and the result of
the last check (up to date, checking, or which release is available).
**Check now** checks right away, even with automatic checks off; once a
release is found, the same link downloads, installs or opens it like the
title bar button.

## How it works

Each release carries a signed manifest per platform,
`update-windows-x86_64.json`, `update-macos-universal.json` and
`update-linux-x86_64.json`. Loog downloads it from
`https://github.com/lius-new/loog/releases/latest/download/` (the newest
release that is not a pre-release) and:

1. verifies its Ed25519 signature with the public key built into Loog
   (`src/infrastructure/update.rs`), so only manifests signed with the
   release key are trusted;
2. checks that it names this platform and a version newer than the running
   one;
3. downloads the package for this kind of install and checks its size and
   SHA-256 against the manifest before it is ever used.

Requests honor `HTTPS_PROXY`/`HTTP_PROXY` and use HTTPS only. Check failures
are silent; the next check tries again. A failed download shows an error and
offers the update again.

## Releasing

The release workflows run `node scripts/package/package.mjs manifest` after
building the packages and publish `update-<platform>.json` with them. The step
needs the `LOOG_UPDATE_SIGNING_KEY` repository secret (the PKCS#8 PEM private
key) and fails without it. See
[Packaging](../scripts/package/README.md#update-manifests) for creating or
replacing the key.

## Testing against a local feed

`LOOG_UPDATE_FEED=<base url>` makes any build, including a debug build, read
its manifest from another place. A loopback feed may use plain HTTP and skips
the proxy:

```bash
LOOG_UPDATE_FEED=http://127.0.0.1:8765 node scripts/build/cargo.mjs run
```

The manifest must still be signed with the release key, and its package URLs
must use HTTPS. A debug build counts as a Windows installer install only when
an `unins000.exe` file sits next to `Loog.exe`; on Linux it counts as a
`.tar.gz` install of `target/debug`. To check that a published feed verifies:

```bash
node scripts/build/cargo.mjs test live_feed -- --ignored --nocapture
```
