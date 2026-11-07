import test from 'node:test';
import assert from 'node:assert/strict';
import {allowed, violations} from './check.mjs';
const policy = () => ({exceptions: {}, root_state_adapters: [], runtime_bridges: [], state_ownership: {}});

test('Markdown external parser is confined to its adapter', () => {
  assert.equal(allowed('src/features/markdown/parser.rs', 'pulldown_cmark'), true);
  for (const file of ['src/features/markdown/model.rs', 'src/ui/markdown/view.rs', 'src/domain/workspace.rs']) {
    assert.equal(allowed(file, 'pulldown_cmark'), false);
  }
  assert.deepEqual(violations({'src/features/markdown/parser.rs': 'use pulldown_cmark::{Parser, Event};'}, policy()), []);
});

test('Markdown syntax contracts reject workspace and renderer identities', () => {
  for (const name of ['FileId', 'ViewId', 'PaneId', 'DocumentStamp', 'UiRect']) {
    assert.ok(violations({'src/features/markdown/model.rs': `struct Model { target: ${name} }`}, policy()).some(e => e.includes('syntax models')));
  }
});

test('Markdown rules and views reject root state and native resources', () => {
  for (const file of ['src/features/markdown.rs', 'src/features/markdown/model.rs', 'src/ui/markdown.rs', 'src/ui/markdown/view.rs', 'src/ui/markdown_binding.rs']) {
    for (const source of ['use crate::app::context::AppContext;', 'use std::{fs, thread};', 'use std::net::TcpStream;', 'use std::{io, env};', 'struct Frame { root: AppState }', 'struct Frame { resource: Mutex<()> }', 'use crate::infrastructure::update;']) {
      assert.ok(violations({[file]: source}, policy()).some(e => e.includes('Markdown rules/views')));
    }
  }
});

test('Markdown modules cannot regain capabilities through includes or exceptions', () => {
  for (const file of ['src/features/markdown/model.rs', 'src/ui/markdown/view.rs']) {
    for (const source of ['include!("../../app/state.rs");', '#[path = "../../app/state.rs"] mod state;', 'use super::super::app;']) {
      assert.ok(violations({[file]: source}, policy()).some(e => e.includes('Markdown modules')));
    }
    const p = policy();
    p.exceptions[file] = {dependencies: ['crate::app'], reason: 'forbidden shortcut', remove_in: 'P2'};
    assert.ok(violations({[file]: 'use crate::app::state;'}, p).some(e => e.includes('Markdown rules/views')));
  }
});

test('Markdown reading frames may use GUI geometry and semantic callback ports', () => {
  assert.deepEqual(violations({'src/ui/markdown/view.rs': 'use lgui::prelude::{Element, UiRect}; use crate::features::markdown; struct Frame { view: ViewId }'}, policy()), []);
});

test('host modules cannot include the independent P0 prototype', () => {
  assert.ok(violations({'src/app/markdown.rs': '#[path = "../../tests/markdown-p0/src/lib.rs"] mod probe;'}, policy()).some(e => e.includes('P0 prototypes')));
});

test('pure Markdown verification compiles production sources without a second parser or GUI dependencies', async () => {
  const fs = await import('node:fs/promises');
  const manifest = await fs.readFile(new URL('../../crates/markdown-tests/Cargo.toml', import.meta.url), 'utf8');
  assert.doesNotMatch(manifest, /lgui|skia|loog-editor-core|build-dependencies/);
  const entry = await fs.readFile(new URL('../../crates/markdown-tests/src/lib.rs', import.meta.url), 'utf8');
  for (const file of ['content', 'layout', 'model', 'parser', 'reader', 'resources', 'production_tests']) assert.ok(entry.includes('src/features/markdown/' + file + '.rs'));
  assert.doesNotMatch(entry, /fn parse|struct Markdown|std::fs|std::thread/);
});
