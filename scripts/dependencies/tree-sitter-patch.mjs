#!/usr/bin/env node
// Keeps vendor/tree-sitter equal to upstream plus vendor/tree-sitter/LOOG.patch.
//
//   node scripts/dependencies/tree-sitter-patch.mjs check       fail if the vendored crate drifted
//   node scripts/dependencies/tree-sitter-patch.mjs regenerate  rewrite the patch from vendor/tree-sitter
//
// Upstream comes from the Cargo registry cache when present, otherwise from crates.io with
// a checksum check. See vendor/tree-sitter/LOOG-PATCH.md for the upgrade procedure.
import { execFileSync } from "node:child_process";
import { createHash } from "node:crypto";
import { cpSync, existsSync, mkdtempSync, readdirSync, readFileSync, rmSync, statSync, writeFileSync } from "node:fs";
import { homedir, tmpdir } from "node:os";
import { join, resolve } from "node:path";

const root = resolve(import.meta.dirname, "..", "..");
const vendor = join(root, "vendor", "tree-sitter");
const ignored = new Set([".cargo-ok", ".cargo_vcs_info.json", "LOOG-PATCH.md", "LOOG.patch"]);

const version = /^version\s*=\s*"([^"]+)"/m.exec(readFileSync(join(vendor, "Cargo.toml"), "utf8"))?.[1];
if (!version) throw new Error("cannot read the vendored tree-sitter version");
const patchFile = join(vendor, "LOOG.patch");

// crates.io checksum (the Cargo.lock "checksum") of each upstream version this script can
// download. Add the new version's checksum when upgrading.
const checksums = { "0.27.2": "37b2bed8f39a73439f3b0b7ed8f452be5d2911b5e9da69d352ef113bb7448fb8" };

// Uses the Cargo registry cache when it has the crate (set TREE_SITTER_PATCH_DOWNLOAD=1 to
// skip it); otherwise downloads the crate from crates.io and verifies its checksum. The
// vendored path patch removes tree-sitter from Cargo.lock's registry sources, so
// `cargo fetch` does not provide it.
async function upstream(work) {
  const registry = join(process.env.CARGO_HOME ?? join(homedir(), ".cargo"), "registry", "src");
  const cached = existsSync(registry) && !process.env.TREE_SITTER_PATCH_DOWNLOAD ? readdirSync(registry) : [];
  for (const index of cached) {
    const dir = join(registry, index, `tree-sitter-${version}`);
    if (existsSync(dir)) return dir;
  }
  const expected = checksums[version];
  if (!expected) throw new Error(`no crates.io checksum recorded for tree-sitter ${version}; add it to \`checksums\``);
  const response = await fetch(`https://static.crates.io/crates/tree-sitter/tree-sitter-${version}.crate`);
  if (!response.ok) throw new Error(`downloading tree-sitter ${version}: HTTP ${response.status}`);
  const archive = Buffer.from(await response.arrayBuffer());
  const actual = createHash("sha256").update(archive).digest("hex");
  if (actual !== expected) throw new Error(`tree-sitter ${version} checksum mismatch: ${actual}`);
  writeFileSync(join(work, "upstream.crate"), archive);
  execFileSync("tar", ["-xzf", "upstream.crate"], { cwd: work });
  return join(work, `tree-sitter-${version}`);
}
const git = (cwd, ...args) =>
  execFileSync("git", ["-c", "core.autocrlf=false", "-c", "core.safecrlf=false", ...args], { cwd, encoding: "utf8", maxBuffer: 1 << 28 });
function copy(from, to) {
  cpSync(from, to, { recursive: true });
  for (const name of ignored) rmSync(join(to, name), { force: true });
}
function files(dir, prefix = "") {
  return readdirSync(dir).flatMap((name) => {
    const rel = prefix ? `${prefix}/${name}` : name;
    return statSync(join(dir, name)).isDirectory() ? files(join(dir, name), rel) : [rel];
  });
}

const work = mkdtempSync(join(tmpdir(), "tree-sitter-patch-"));
try {
  const a = join(work, "a");
  const b = join(work, "b");
  copy(await upstream(work), a);
  copy(vendor, b);
  if (process.argv[2] === "regenerate") {
    let diff = "";
    try {
      git(work, "diff", "--no-index", "--no-color", "--src-prefix=a/", "--dst-prefix=b/", "a", "b");
    } catch (error) {
      diff = error.stdout ?? ""; // git exits 1 when the trees differ
    }
    // `git diff --no-index` writes "a/a/..." style paths relative to the work directory; normalize them.
    diff = diff.replaceAll("a/a/", "a/").replaceAll("b/b/", "b/");
    writeFileSync(patchFile, diff);
    console.log(`wrote ${patchFile}`);
  } else if (process.argv[2] === "check") {
    // Apply the stored patch to pristine upstream and require byte equality with the vendored copy.
    const applied = join(work, "applied");
    copy(await upstream(work), applied);
    git(applied, "apply", "--unsafe-paths", "-p1", patchFile);
    const left = files(applied).sort();
    const right = files(b).sort();
    const bad = [...new Set([...left, ...right])].filter(
      (f) => !left.includes(f) || !right.includes(f) || !readFileSync(join(applied, f)).equals(readFileSync(join(b, f))),
    );
    if (bad.length) {
      console.error(`vendor/tree-sitter differs from upstream ${version} + ${patchFile}:\n  ${bad.join("\n  ")}`);
      console.error("Run `node scripts/dependencies/tree-sitter-patch.mjs regenerate` if the change is intended.");
      process.exitCode = 1;
    } else console.log(`vendor/tree-sitter matches upstream ${version} + patch`);
  } else {
    console.error("usage: tree-sitter-patch.mjs check|regenerate");
    process.exitCode = 2;
  }
} finally {
  rmSync(work, { recursive: true, force: true });
}
