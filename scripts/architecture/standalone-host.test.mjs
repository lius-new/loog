import test from 'node:test';
import assert from 'node:assert/strict';
import {hostFile} from './standalone-host.mjs';
test('standalone snapshot excludes every language implementation and keeps host unit fixtures',()=>{
  for(const file of ['tools/ext-validate/Cargo.toml','tools/ext-validate/tests/cli.rs','language-packs/Cargo.toml','language-packs/grammars/json/src/lib.rs','language-packs/queries/rust/highlights.scm','crates/language-packs/rust/Cargo.toml','tests/fixtures/extensions/loog.lang.json/tree-sitter.json','assets/grammars/markdown/highlights.scm'])assert.equal(hostFile(file),false,file);
  for(const file of ['Cargo.toml','Cargo.lock','build.rs','src/main.rs','tests/fixtures/language_registry.rs','crates/editor-core/Cargo.toml'])assert.equal(hostFile(file),true,file);
});
