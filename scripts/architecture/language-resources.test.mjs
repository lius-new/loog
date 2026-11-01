import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {createHash} from 'node:crypto';
import {runtimeGrammarManifestErrors} from './check.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
test('the host is independent of the language producer and concrete grammar crates', () => {
  for (const directory of ['language-packs', 'crates/language-packs']) assert.equal(fs.existsSync(path.join(root, directory)), false, directory);
  for (const file of ['Cargo.toml', 'crates/editor-core/Cargo.toml', 'crates/workspace-tests/Cargo.toml', 'tests/language-runtime/Cargo.toml']) {
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
    assert.deepEqual(extension.contributes.languages.map(language => language.path), ['tree-sitter.json']);
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
  for (const manifest of ['Cargo.toml', 'tests/language-runtime/Cargo.toml']) {
    assert.match(fs.readFileSync(path.join(root, manifest), 'utf8'), /tree-sitter\s*=\s*\{[^}]*features\s*=\s*\[[^\]]*"wasm"/);
  }
  for (const workflow of ['ci.yml', 'validation.yml', 'release.yml']) {
    const text = fs.readFileSync(path.join(root, '.github/workflows', workflow), 'utf8');
    assert.doesNotMatch(text, /language-packs\/build|loom-grammar-/);
    assert.doesNotMatch(text, /fetch-languages\.mjs|LOOM_LANGUAGE_PACKS/);
    if (workflow === 'validation.yml') assert.match(text, /run-wasm\.mjs/, 'Optional WASM verification remains available without a native resource feed');
    if (workflow === 'release.yml') assert.doesNotMatch(text, /run-wasm\.mjs/, 'Release reuses exact-commit CI verification');
  }
  assert.match(fs.readFileSync(path.join(root, 'scripts/package/macos/entitlements.plist'), 'utf8'), /com\.apple\.security\.cs\.allow-jit/);
});

test('Rust regression metadata and queries are the unmodified v0.24.2 upstream resources', () => {
  for (const [file, digest] of Object.entries({
    'tree-sitter.json': '1ff4c338e5f9a8ff725f2f37b2e125dceac46a139cd572064d8dcfbbef9c0aaf',
    'queries/highlights.scm': '0f0343107f14a7690157f51090a979eb8f8bfe4eada7c61763ddb4c54b1311d1',
    'queries/injections.scm': '723146f179bc0edfba2f51731c99afce8bed3b677dfb4435c303859fae299ba5',
  })) assert.equal(createHash('sha256').update(fs.readFileSync(path.join(root, 'tests/language-runtime/fixtures/rust', file))).digest('hex'), digest, file);
});
