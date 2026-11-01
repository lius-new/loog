// The extensions Loom ships come from the extensions repository: `extensions.lock`
// (written there by `loom-ext lock`) names each one's version and package with its
// SHA-256, and `index.snapshot.json` is the signed index it was locked against, which
// the application falls back to when the live index cannot be fetched.
//
// Both live in assets/registry and are refreshed with each application release.
// While there is no lock, the folders in assets/extensions are shipped as they are.
//
//   node scripts/package/fetch-locked.mjs    download and check the locked packages
//
// The packaging scripts then stage them (`stageBundled`) like any other bundled
// extension, with the same checks.
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {stageExtensions} from './extensions.mjs';

const ROOT = fileURLToPath(new URL('../../../', import.meta.url));
export const REGISTRY_DIR = path.join(ROOT, 'assets/registry');
export const LOCK_FILE = path.join(REGISTRY_DIR, 'extensions.lock');
export const LOCKED_DIR = path.join(ROOT, '.packaging/locked');
const SHIPPED = ['extensions.lock', 'index.snapshot.json', 'index.snapshot.json.sig'];
const ID = /^[a-z0-9][a-z0-9-]*(\.[a-z0-9][a-z0-9-]*)+$/;

const sha256 = bytes => crypto.createHash('sha256').update(bytes).digest('hex');

/** The lock in `file`, checked for what packaging relies on; `null` when there is none. */
export function readLock(file = LOCK_FILE) {
  if (!fs.existsSync(file)) return null;
  const lock = JSON.parse(fs.readFileSync(file, 'utf8'));
  if (lock.schema !== 1 || !Array.isArray(lock.extensions) || !lock.extensions.length) throw new Error(`${file}: not a usable lock`);
  for (const extension of lock.extensions) {
    if (!ID.test(extension.id ?? '') || !Array.isArray(extension.artifacts)) throw new Error(`${file}: invalid entry ${JSON.stringify(extension.id)}`);
  }
  return lock;
}

async function download(url, size) {
  if (!url.startsWith('https://')) throw new Error(`${url} is not an https address`);
  const response = await fetch(url);
  if (!response.ok) throw new Error(`${url}: HTTP ${response.status}`);
  const bytes = Buffer.from(await response.arrayBuffer());
  if (bytes.length !== size) throw new Error(`${url}: expected ${size} bytes, got ${bytes.length}`);
  return bytes;
}

/**
 * Download each locked package (the one for any platform: what Loom ships is data),
 * check it against the lock, and unpack it into `destination/<id>`.
 */
export async function fetchLocked({lock, destination, fetchBytes = download}) {
  fs.rmSync(destination, {recursive: true, force: true});
  fs.mkdirSync(destination, {recursive: true});
  const work = fs.mkdtempSync(path.join(os.tmpdir(), 'loom-locked-'));
  try {
    for (const extension of lock.extensions) {
      const artifact = extension.artifacts.find(candidate => candidate.platform === 'any');
      if (!artifact) throw new Error(`${extension.id} has no package for every platform; only such extensions can be bundled`);
      const bytes = await fetchBytes(artifact.url, artifact.size);
      if (bytes.length !== artifact.size || sha256(bytes) !== artifact.sha256) throw new Error(`${extension.id} ${extension.version}: the package does not match the lock`);
      const archive = path.join(work, `${extension.id}.loomext`);
      fs.writeFileSync(archive, bytes);
      const names = execFileSync('tar', ['-tzf', archive], {encoding: 'utf8', maxBuffer: 64 * 1024 * 1024}).split(/\r?\n/).filter(Boolean);
      for (const name of names) {
        const parts = name.replace(/\/$/, '').split('/');
        if (parts[0] !== extension.id || parts.includes('..') || path.isAbsolute(name) || /[\\:]/.test(name)) throw new Error(`${extension.id}: the package has an unexpected entry ${name}`);
      }
      execFileSync('tar', ['-xzf', archive, '-C', destination]);
      if (!fs.existsSync(path.join(destination, extension.id, 'manifest.json'))) throw new Error(`${extension.id}: the package has no manifest.json`);
    }
  } finally {
    fs.rmSync(work, {recursive: true, force: true});
  }
  return lock.extensions.map(extension => extension.id);
}

/**
 * Stage the bundled extensions into `destination` (the `extensions` folder of the
 * application): the locked ones when there is a lock, else assets/extensions. The
 * lock and snapshot go to the `registry` folder beside it.
 */
export function stageBundled({destination, lock = LOCK_FILE, locked = LOCKED_DIR, registry = REGISTRY_DIR}) {
  const found = readLock(lock);
  if (!found) return stageExtensions({destination});
  const missing = found.extensions.filter(extension => !fs.existsSync(path.join(locked, extension.id, 'manifest.json')));
  if (missing.length) throw new Error(`Locked extensions are not fetched (${missing.map(e => e.id).join(', ')}); run node scripts/package/fetch-locked.mjs`);
  const binaries = stageExtensions({source: locked, destination});
  const shipped = path.join(path.dirname(path.resolve(destination)), 'registry');
  fs.mkdirSync(shipped, {recursive: true});
  for (const name of SHIPPED) {
    const from = path.join(registry, name);
    if (!fs.existsSync(from)) throw new Error(`${from} is missing; the lock needs the snapshot it was made against`);
    fs.copyFileSync(from, path.join(shipped, name));
  }
  return binaries;
}
