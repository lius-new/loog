// Packs an extension folder into a `.loomext` package.
//
//   node scripts/extensions/package/pack.mjs <folder> [--out <directory>]
//
// The package is a gzip-compressed tar archive with one root folder, named after
// the extension id, that holds the folder's files, `manifest.json` among them. The output is
// `<id>-<version>.loomext` (default directory: target/extension-packages) and a
// `.sha256` file beside it. The same folder always gives the same bytes.
//
// Everything the extensions page and the import show about an extension has to
// be in the manifest; the checks below mirror what the importer requires, so a
// package that is built here is one that imports. The importer
// (src/infrastructure/extensions/package.rs) is the authority and checks again.
import { createHash } from 'node:crypto';
import { existsSync, lstatSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { dirname, join, relative, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';
import { gzipSync } from 'node:zlib';
import { createTar } from './tar.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..', '..', '..');

export const CATEGORIES = ['Themes', 'Languages', 'Snippets', 'Linters', 'Formatters', 'Editing', 'Source control', 'Productivity', 'Other'];
const MAX_ICON = 256 * 1024;
const MAX_DOCUMENT = 512 * 1024;
const MAX_ARCHIVE = 64 * 1024 * 1024;
const MAX_UNPACKED = 128 * 1024 * 1024;
const MAX_ENTRIES = 2048;
const SKIPPED = new Set(['.git', 'node_modules', '.DS_Store', 'Thumbs.db', '.loomext-source.json']);

const VERSION = /^\d+\.\d+\.\d+([-+][0-9A-Za-z.+-]*)?$/;
const ID = /^[a-z0-9]+(-[a-z0-9]+)*(\.[a-z0-9]+(-[a-z0-9]+)*)+$/;

/** Whether `path` is a safe package-relative path (`/`-separated, no climbing). */
export function safePath(path) {
  if (typeof path !== 'string' || path === '' || path.length > 240) return false;
  if (path.startsWith('/') || /^[A-Za-z]:/.test(path) || path.includes('\\')) return false;
  return path.split('/').every((part) => part !== '' && part !== '.' && part !== '..' && !/[.\s]$/.test(part) && !/[:*?"<>|\u0000-\u001f]/.test(part));
}

/** Every problem that would keep `manifest` from being packaged. `sizeOf(path)` gives a size or undefined. */
export function manifestProblems(manifest, sizeOf) {
  const problems = [];
  const need = (ok, message) => { if (!ok) problems.push(message); };
  const text = (value, field, max) => {
    if (value === undefined) { problems.push(`\`${field}\` is missing`); return; }
    if (typeof value !== 'string' || value.trim() === '' || [...value].length > max || /[\r\n\0]/.test(value)) {
      problems.push(`\`${field}\` must be 1 to ${max} characters on one line`);
    }
  };
  need(manifest.schema === 1, '`schema` must be 1');
  need(typeof manifest.id === 'string' && ID.test(manifest.id), '`id` must look like vendor.name (lowercase words joined by - and .)');
  need(typeof manifest.version === 'string' && VERSION.test(manifest.version), '`version` must look like 1.2.3');
  text(manifest.name, 'name', 60);
  text(manifest.publisher, 'publisher', 60);
  text(manifest.description, 'description', 200);
  text(manifest.license, 'license', 100);
  if (manifest.categories === undefined || !Array.isArray(manifest.categories) || manifest.categories.length === 0) {
    problems.push('`categories` is missing: one to three of ' + CATEGORIES.join(', '));
  } else {
    need(manifest.categories.length <= 3, '`categories` lists more than three');
    for (const category of manifest.categories) need(CATEGORIES.includes(category), `\`${category}\` is not a category; use one of ${CATEGORIES.join(', ')}`);
  }
  if (manifest.keywords !== undefined) {
    need(Array.isArray(manifest.keywords) && manifest.keywords.length <= 10 && manifest.keywords.every((k) => typeof k === 'string' && k.trim() !== '' && [...k].length <= 30), '`keywords` allows at most ten words of up to 30 characters');
  }
  for (const field of ['repository', 'homepage']) {
    const value = manifest[field];
    if (value === undefined) { if (field === 'repository') problems.push('`repository` is missing: an https:// address'); continue; }
    need(typeof value === 'string' && value.startsWith('https://') && value.length <= 300 && !/\s/.test(value), `\`${field}\` must be an https:// address`);
  }
  const documents = [['icon', MAX_ICON, /\.(png|svg)$/i, 'a PNG or SVG file'], ['readme', MAX_DOCUMENT, /\.md$/i, 'a Markdown file'], ['changelog', MAX_DOCUMENT, /\.md$/i, 'a Markdown file']];
  for (const [field, limit, pattern, kind] of documents) {
    const path = manifest[field];
    if (path === undefined) { problems.push(`\`${field}\` is missing: ${kind} in the package`); continue; }
    if (!safePath(path)) { problems.push(`\`${field}\`: ${path} must stay inside the package`); continue; }
    if (!pattern.test(path)) { problems.push(`\`${field}\` must be ${kind}`); continue; }
    const size = sizeOf(path);
    if (size === undefined) problems.push(`\`${field}\`: ${path} is not in the package`);
    else if (size === 0) problems.push(`\`${field}\`: ${path} is empty`);
    else if (size > limit) problems.push(`\`${field}\`: ${path} is larger than ${limit / 1024} KiB`);
  }
  if (manifest.engines === undefined || typeof manifest.engines !== 'object') problems.push('`engines.loom` is missing: the oldest Loom version it runs on');
  else need(typeof manifest.engines.loom === 'string' && /^\d+\.\d+\.\d+$/.test(manifest.engines.loom), '`engines.loom` must look like 0.1.8');
  if (manifest.component !== undefined) {
    if (!safePath(manifest.component)) problems.push(`\`component\`: ${manifest.component} must stay inside the package`);
    else if (sizeOf(manifest.component) === undefined) problems.push(`\`component\`: ${manifest.component} is not in the package`);
  }
  // Data resources that the manifest lists must be in the package too.
  const contributes = manifest.contributes ?? {};
  for (const kind of ['themes', 'highlight', 'snippets']) {
    for (const resource of contributes[kind] ?? []) {
      if (!safePath(resource.path)) problems.push(`\`contributes.${kind}\`: ${resource.path} must stay inside the package`);
      else if (sizeOf(resource.path) === undefined) problems.push(`\`contributes.${kind}\`: ${resource.path} is not in the package`);
    }
  }
  return problems;
}

/** The files of `folder` as `{ path, data }`, sorted, with links and oversized trees refused. */
export function collect(folder) {
  const found = [];
  const walk = (directory) => {
    for (const name of readdirSync(directory).sort()) {
      if (SKIPPED.has(name) || name.endsWith('.loomext') || name.endsWith('.loomext.sha256')) continue;
      const path = join(directory, name);
      const info = lstatSync(path);
      if (info.isSymbolicLink()) throw new Error(`${relative(folder, path)} is a link; a package holds regular files only`);
      if (info.isDirectory()) walk(path);
      else if (info.isFile()) found.push({ path: relative(folder, path).split('\\').join('/'), data: readFileSync(path) });
    }
  };
  walk(folder);
  found.sort((a, b) => (a.path < b.path ? -1 : a.path > b.path ? 1 : 0));
  if (found.length > MAX_ENTRIES) throw new Error(`the folder has more than ${MAX_ENTRIES} files`);
  const seen = new Set();
  let total = 0;
  for (const { path, data } of found) {
    if (!safePath(path)) throw new Error(`${path} cannot be used in a package`);
    if (seen.has(path.toLowerCase())) throw new Error(`${path} has the same name as another file when case is ignored`);
    seen.add(path.toLowerCase());
    total += data.length;
  }
  if (total > MAX_UNPACKED) throw new Error('the folder holds more than 128 MiB');
  return found;
}

/** Build the package for `folder`. Throws an Error whose message lists every problem. */
export function pack(folder, out) {
  const manifestPath = join(folder, 'manifest.json');
  if (!existsSync(manifestPath)) throw new Error(`${folder} has no manifest.json`);
  let manifest;
  try { manifest = JSON.parse(readFileSync(manifestPath, 'utf8')); } catch (error) { throw new Error(`manifest.json is not valid JSON: ${error.message}`); }
  const files = collect(folder);
  const sizes = new Map(files.map((f) => [f.path, f.data.length]));
  const problems = manifestProblems(manifest, (path) => sizes.get(path));
  if (problems.length) throw new Error(`the manifest is not ready to package:\n- ${problems.join('\n- ')}`);
  // One root folder, named after the extension, holds everything.
  const archive = gzipSync(createTar(files.map((f) => ({ ...f, path: `${manifest.id}/${f.path}` }))), { level: 9 });
  if (archive.length > MAX_ARCHIVE) throw new Error('the package is larger than 64 MiB');
  mkdirSync(out, { recursive: true });
  const file = join(out, `${manifest.id}-${manifest.version}.loomext`);
  const sha256 = createHash('sha256').update(archive).digest('hex');
  writeFileSync(file, archive);
  writeFileSync(`${file}.sha256`, `${sha256}  ${manifest.id}-${manifest.version}.loomext\n`);
  return { file, sha256, size: archive.length, files: files.length, id: manifest.id, version: manifest.version, name: manifest.name };
}

if (process.argv[1] && resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const args = process.argv.slice(2);
  const outAt = args.indexOf('--out');
  const out = resolve(outAt >= 0 ? args.splice(outAt, 2)[1] ?? '' : join(root, 'target', 'extension-packages'));
  const folder = args[0];
  if (!folder) {
    console.error('usage: node scripts/extensions/package/pack.mjs <folder> [--out <directory>]');
    process.exit(2);
  }
  try {
    const built = pack(resolve(folder), out);
    console.log(`${built.name} ${built.version}: ${built.files} files, ${built.size} bytes`);
    console.log(`  ${built.file}`);
    console.log(`  sha256 ${built.sha256}`);
  } catch (error) {
    console.error(error.message);
    process.exit(1);
  }
}
