import {fileURLToPath} from 'node:url';
import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {stageExtensions} from './extensions.mjs';

function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'loog-packaging-extensions-'));
  t.after(() => fs.rmSync(root, {recursive: true, force: true}));
  const source = path.join(root, 'source');
  const write = (file, text) => {
    fs.mkdirSync(path.dirname(path.join(source, file)), {recursive: true});
    fs.writeFileSync(path.join(source, file), text);
  };
  write('loog.look/manifest.json', JSON.stringify({schema: 1, id: 'loog.look', version: '1.0.0', contributes: {themes: [{id: 'loog.look.a', path: 'a.json'}]}}));
  write('loog.look/a.json', '{}');
  write('loog.lang.rust/manifest.json', JSON.stringify({schema: 1, id: 'loog.lang.rust', version: '1.0.0', contributes: {highlight: [{id: 'loog.lang.rust.grammar', path: 'tree-sitter.json'}]}}));
  write('loog.lang.rust/tree-sitter.json', JSON.stringify({grammars: [{name: 'rust', 'file-types': ['rs']}]}));
  write('loog.lang.rust/tree-sitter-rust.wasm', '\0asm');
  write('loog.lang.rust/queries/highlights.scm', '(identifier) @variable');
  return {root, source, write, destination: path.join(root, 'staged')};
}

test('every extension is copied whole', t => {
  const {source, destination} = fixture(t);
  const binaries = stageExtensions({source, destination});
  assert.deepEqual(binaries, [path.join(destination, 'loog.lang.rust', 'tree-sitter-rust.wasm')]);
  assert.equal(fs.readFileSync(path.join(destination, 'loog.look/a.json'), 'utf8'), '{}');
  assert.equal(fs.existsSync(path.join(destination, 'loog.lang.rust/queries/highlights.scm')), true);
});

test('a broken package stops packaging and leaves nothing of itself behind', t => {
  const {source, destination, write} = fixture(t);
  fs.unlinkSync(path.join(source, 'loog.lang.rust/tree-sitter-rust.wasm'));
  assert.throws(() => stageExtensions({source, destination}), /Missing prebuilt language module: rust/);
  assert.equal(fs.existsSync(path.join(destination, 'loog.lang.rust')), false);

  // A native library is not a grammar.
  write('loog.lang.rust/rust.dll', 'native');
  assert.throws(() => stageExtensions({source, destination}), /Missing prebuilt language module/);

  write('loog.lang.rust/tree-sitter-rust.wasm', '\0asm');
  write('loog.lang.rust/manifest.json', JSON.stringify({schema: 1, id: 'loog.other', version: '1.0.0'}));
  assert.throws(() => stageExtensions({source, destination}), /Invalid extension manifest/);
});

test('resource paths cannot leave the package and an empty input is refused', t => {
  const {source, destination, write} = fixture(t);
  write('loog.lang.rust/manifest.json', JSON.stringify({schema: 1, id: 'loog.lang.rust', version: '1.0.0', contributes: {highlight: [{id: 'loog.lang.rust.grammar', path: '../tree-sitter.json'}]}}));
  assert.throws(() => stageExtensions({source, destination}), /Invalid extension resource path/);
  assert.throws(() => stageExtensions({source: '', destination}), /Supply/);
  const empty = path.join(fixture(t).root, 'empty');
  fs.mkdirSync(empty);
  assert.throws(() => stageExtensions({source: empty, destination}), /No bundled extensions/);
});

test('the copies of the bundled extensions used by tests stage cleanly', t => {
  const destination = fs.mkdtempSync(path.join(os.tmpdir(), 'loog-packaging-real-'));
  t.after(() => fs.rmSync(destination, {recursive: true, force: true}));
  const source = fileURLToPath(new URL('../../../tests/fixtures/extensions', import.meta.url));
  const binaries = stageExtensions({source, destination});
  const names = binaries.map(file => path.basename(file)).sort();
  assert.deepEqual(names, ['tree-sitter-json.wasm', 'tree-sitter-markdown.wasm', 'tree-sitter-markdown_inline.wasm']);
  for (const id of ['loog.aura', 'loog.lang.json', 'loog.lang.markdown']) {
    assert.equal(fs.existsSync(path.join(destination, id, 'manifest.json')), true, id);
  }
});
