import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {stageExtensions} from './extensions.mjs';

function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'loom-packaging-extensions-'));
  t.after(() => fs.rmSync(root, {recursive: true, force: true}));
  const source = path.join(root, 'source');
  const write = (file, text) => {
    fs.mkdirSync(path.dirname(path.join(source, file)), {recursive: true});
    fs.writeFileSync(path.join(source, file), text);
  };
  write('loom.look/manifest.json', JSON.stringify({schema: 1, id: 'loom.look', version: '1.0.0', contributes: {themes: [{id: 'loom.look.a', path: 'a.json'}]}}));
  write('loom.look/a.json', '{}');
  write('loom.lang.rust/manifest.json', JSON.stringify({schema: 1, id: 'loom.lang.rust', version: '1.0.0', contributes: {languages: [{id: 'loom.lang.rust.grammar', path: 'tree-sitter.json'}]}}));
  write('loom.lang.rust/tree-sitter.json', JSON.stringify({grammars: [{name: 'rust', 'file-types': ['rs']}]}));
  write('loom.lang.rust/tree-sitter-rust.wasm', '\0asm');
  write('loom.lang.rust/queries/highlights.scm', '(identifier) @variable');
  write('registry.toml', 'schema = 1\n');
  return {root, source, write, destination: path.join(root, 'staged')};
}

test('every extension is copied whole, with the registry beside them', t => {
  const {source, destination} = fixture(t);
  const binaries = stageExtensions({source, destination});
  assert.deepEqual(binaries, [path.join(destination, 'loom.lang.rust', 'tree-sitter-rust.wasm')]);
  assert.equal(fs.readFileSync(path.join(destination, 'loom.look/a.json'), 'utf8'), '{}');
  assert.equal(fs.existsSync(path.join(destination, 'loom.lang.rust/queries/highlights.scm')), true);
  assert.equal(fs.readFileSync(path.join(destination, 'registry.toml'), 'utf8'), 'schema = 1\n');
});

test('a broken package stops packaging and leaves nothing of itself behind', t => {
  const {source, destination, write} = fixture(t);
  fs.unlinkSync(path.join(source, 'loom.lang.rust/tree-sitter-rust.wasm'));
  assert.throws(() => stageExtensions({source, destination}), /Missing prebuilt language module: rust/);
  assert.equal(fs.existsSync(path.join(destination, 'loom.lang.rust')), false);

  // A native library is not a grammar.
  write('loom.lang.rust/rust.dll', 'native');
  assert.throws(() => stageExtensions({source, destination}), /Missing prebuilt language module/);

  write('loom.lang.rust/tree-sitter-rust.wasm', '\0asm');
  write('loom.lang.rust/manifest.json', JSON.stringify({schema: 1, id: 'loom.other', version: '1.0.0'}));
  assert.throws(() => stageExtensions({source, destination}), /Invalid extension manifest/);
});

test('resource paths cannot leave the package and an empty input is refused', t => {
  const {source, destination, write} = fixture(t);
  write('loom.lang.rust/manifest.json', JSON.stringify({schema: 1, id: 'loom.lang.rust', version: '1.0.0', contributes: {languages: [{id: 'loom.lang.rust.grammar', path: '../tree-sitter.json'}]}}));
  assert.throws(() => stageExtensions({source, destination}), /Invalid extension resource path/);
  assert.throws(() => stageExtensions({source: '', destination}), /Supply/);
  const empty = path.join(fixture(t).root, 'empty');
  fs.mkdirSync(empty);
  assert.throws(() => stageExtensions({source: empty, destination}), /No bundled extensions/);
});

test('the extensions in the repository stage cleanly', t => {
  const destination = fs.mkdtempSync(path.join(os.tmpdir(), 'loom-packaging-real-'));
  t.after(() => fs.rmSync(destination, {recursive: true, force: true}));
  const binaries = stageExtensions({destination});
  const names = binaries.map(file => path.basename(file)).sort();
  assert.deepEqual(names, ['tree-sitter-json.wasm', 'tree-sitter-markdown.wasm', 'tree-sitter-markdown_inline.wasm']);
  for (const id of ['loom.aura', 'loom.lang.json', 'loom.lang.markdown']) {
    assert.equal(fs.existsSync(path.join(destination, id, 'manifest.json')), true, id);
  }
  assert.equal(fs.existsSync(path.join(destination, 'registry.toml')), true);
});
