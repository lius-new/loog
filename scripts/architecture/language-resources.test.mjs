import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {runtimeGrammarManifestErrors} from './check.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
test('the host is independent of the language producer and concrete grammar crates', () => {
  for (const directory of ['language-packs', 'crates/language-packs']) assert.equal(fs.existsSync(path.join(root, directory)), false, directory);
  for (const file of ['Cargo.toml', 'crates/editor-core/Cargo.toml', 'crates/workspace-tests/Cargo.toml', 'tools/ext-validate/Cargo.toml']) {
    assert.deepEqual(runtimeGrammarManifestErrors(fs.readFileSync(path.join(root, file), 'utf8')), [], file);
  }
  assert.equal(fs.existsSync(path.join(root, 'assets/language-packs')), false, 'language packs are extensions now');
  const resources = path.join(root, 'tests/fixtures/extensions');
  // The grammars that ship with Loom, each inside its own extension package.
  const languages = fs.readdirSync(resources).filter(id => fs.existsSync(path.join(resources, id, 'tree-sitter.json'))).sort();
  assert.deepEqual(languages, ['loom.lang.json', 'loom.lang.markdown']);
  for (const id of languages) {
    const extension = JSON.parse(fs.readFileSync(path.join(resources, id, 'manifest.json')));
    assert.equal(extension.id, id);
    assert.deepEqual(extension.contributes.highlight.map(language => language.path), ['tree-sitter.json']);
    const manifest = JSON.parse(fs.readFileSync(path.join(resources, id, 'tree-sitter.json')));
    assert.equal(fs.existsSync(path.join(resources, id, 'package.json')), false);
    assert.ok(Array.isArray(manifest.grammars));
    for (const grammar of manifest.grammars) {
      assert.match(grammar.name, /^[a-z][a-z0-9_]*$/);
      const module = fs.readFileSync(path.join(resources, id, `tree-sitter-${grammar.name}.wasm`));
      assert.deepEqual([...module.subarray(0, 8)], [0, 97, 115, 109, 1, 0, 0, 0]);
      for (const file of [grammar.highlights].flat()) assert.ok(fs.statSync(path.join(resources, id, file)).size > 0);
    }
  }
  for (const manifest of ['Cargo.toml', 'tools/ext-validate/Cargo.toml']) {
    assert.match(fs.readFileSync(path.join(root, manifest), 'utf8'), /tree-sitter\s*=\s*\{[^}]*features\s*=\s*\[[^\]]*"wasm"/);
  }
  for (const workflow of ['ci.yml', 'validation.yml', 'release.yml']) {
    const text = fs.readFileSync(path.join(root, '.github/workflows', workflow), 'utf8');
    assert.doesNotMatch(text, /language-packs\/build|loom-grammar-/);
    assert.doesNotMatch(text, /fetch-languages\.mjs|LOOM_LANGUAGE_PACKS/);
    assert.doesNotMatch(text, /run-wasm\.mjs/, 'the language runtime suites are gone; extensions are checked by loom-ext-validate');
  }
  assert.match(fs.readFileSync(path.join(root, 'scripts/package/macos/entitlements.plist'), 'utf8'), /com\.apple\.security\.cs\.allow-jit/);
});
