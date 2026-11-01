// Stage the extensions that ship with Loom. Every folder in `source` (the locked
// packages, unpacked) is a complete extension package: it is copied as it is, after checks that make a
// broken or unsafe package stop packaging rather than ship. This module never
// builds extension code.
import fs from 'node:fs';
import path from 'node:path';

const MAX_FILES = 4096;
const MAX_BYTES = 256 * 1024 * 1024;
const MAX_MANIFEST = 1024 * 1024;
const ID = /^[a-z0-9][a-z0-9-]*(\.[a-z0-9][a-z0-9-]*)+$/;
const NAME = /^[a-z][a-z0-9_]*$/;

function relative(file) {
  if (typeof file !== 'string' || !file || file.startsWith('/') || /[\\:\0]/.test(file) || file.split('/').includes('..')) throw new Error('Invalid extension resource path: ' + file);
  return file;
}

function readJson(file, what) {
  if (fs.statSync(file).size > MAX_MANIFEST) throw new Error(what + ' exceeds byte limit');
  return JSON.parse(fs.readFileSync(file, 'utf8'));
}

// Copy a package folder. Only regular files and folders are shipped: a link could
// point outside the package, and a device or socket has no place in one.
function copyTree(source, target, totals) {
  fs.mkdirSync(target, {recursive: true});
  for (const entry of fs.readdirSync(source, {withFileTypes: true})) {
    const input = path.join(source, entry.name), output = path.join(target, entry.name);
    if (entry.isDirectory()) copyTree(input, output, totals);
    else if (entry.isFile()) {
      totals.files += 1;
      totals.bytes += fs.statSync(input).size;
      if (totals.files > MAX_FILES || totals.bytes > MAX_BYTES) throw new Error('Bundled extensions exceed packaging limits');
      fs.copyFileSync(input, output);
    } else throw new Error('Extension package may contain only files and folders: ' + input);
  }
}

// The WebAssembly module of every grammar a highlight resource names. Grammars are
// run in a sandbox, so a native library is never accepted in its place.
function grammarModules(folder, manifest) {
  const modules = [];
  for (const resource of manifest.contributes?.highlight ?? []) {
    const pack = relative(resource.path);
    if (path.posix.basename(pack) !== 'tree-sitter.json') continue;
    const directory = path.join(folder, path.posix.dirname(pack));
    const standard = readJson(path.join(directory, 'tree-sitter.json'), 'Language pack manifest');
    if (!Array.isArray(standard.grammars) || !standard.grammars.length || standard.grammars.length > 128) throw new Error('Invalid language pack grammars in ' + folder);
    const names = new Set();
    for (const grammar of standard.grammars) {
      const name = grammar.name;
      if (typeof name !== 'string' || !NAME.test(name) || name === 'plaintext' || names.has(name)) throw new Error('Invalid or duplicate grammar name in ' + folder);
      names.add(name);
      const base = relative(grammar.path ?? '.');
      const file = [`tree-sitter-${name}.wasm`, `${name}.wasm`]
        .flatMap(candidate => [candidate, path.posix.join(base, candidate)])
        .find(candidate => fs.statSync(path.join(directory, candidate), {throwIfNoEntry: false})?.isFile());
      if (!file) throw new Error('Missing prebuilt language module: ' + name);
      const module = path.join(directory, file);
      if (!modules.includes(module)) modules.push(module);
    }
  }
  return modules;
}

export function stageExtensions({source, destination}) {
  if (!source) throw new Error('Supply the bundled extensions');
  const root = path.resolve(source);
  const ids = fs.readdirSync(root, {withFileTypes: true})
    .filter(entry => entry.isDirectory() && fs.existsSync(path.join(root, entry.name, 'manifest.json')))
    .map(entry => entry.name).sort();
  if (!ids.length) throw new Error('No bundled extensions supplied');
  const totals = {files: 0, bytes: 0};
  const binaries = [];
  for (const id of ids) {
    const input = path.join(root, id);
    const manifest = readJson(path.join(input, 'manifest.json'), 'Extension manifest');
    if (manifest.schema !== 1 || manifest.id !== id || !ID.test(id)) throw new Error('Invalid extension manifest: ' + id);
    // Validate before copying, so a rejected package leaves nothing behind.
    const modules = grammarModules(input, manifest);
    const output = path.join(destination, id);
    copyTree(input, output, totals);
    for (const module of modules) binaries.push(path.join(output, path.relative(input, module)));
  }
  return binaries;
}
