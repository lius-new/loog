import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {execFileSync} from 'node:child_process';
import test from 'node:test';
import {generateReleaseNotes, parseReleaseNotes} from './notes.mjs';
import {createDraft, prepareDraft, prepareUpload, uploadToDraft} from './publish.mjs';

function fixture(t) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'loog-release-'));
  t.after(() => {
    const resolved = path.resolve(directory);
    assert.equal(path.dirname(resolved), path.resolve(os.tmpdir()));
    assert.ok(path.basename(resolved).startsWith('loog-release-'));
    fs.rmSync(resolved, {recursive: true, force: true});
  });
  const git = (...args) => execFileSync('git', ['-C', directory, ...args], {encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe']}).trim();
  git('init', '-b', 'main');
  git('config', 'user.name', 'Release tests');
  git('config', 'user.email', 'release-tests@example.invalid');
  git('config', 'commit.gpgsign', 'false');
  const commit = body => { git('commit', '--allow-empty', '-m', body); return git('rev-parse', 'HEAD'); };
  commit('Initial version\n\nRelease Notes:\n\n- Added the old feature.');
  git('tag', 'v0.1.2');
  return {directory, git, commit};
}

test('only the Release Notes section supplies text, with wrapped bullets and Git trailers excluded', () => {
  const result = parseReleaseNotes('Technical summary\n\nRelease Notes:\n\n- Added selectable documentation\n  across cards.\n- Fixed labels.\n- Upgrade: Replace `ask` with `manual`.\n\nCo-authored-by: Someone <email@example.invalid>');
  assert.deepEqual(result.entries, [
    {group: 'Added', text: 'Added selectable documentation across cards.'},
    {group: 'Fixed', text: 'Fixed labels.'},
    {group: 'Upgrade notes', text: 'Replace `ask` with `manual`.'},
  ]);
  assert.equal(parseReleaseNotes('Example:\n```\nRelease Notes:\n\n- Added fake text.\n```').present, false);
  assert.deepEqual(parseReleaseNotes('Release Notes:\r\n\r\n- N/A\r\n'), {present: true, entries: []});
  assert.deepEqual(parseReleaseNotes('## Release Notes\n\n- Improved scrolling.\n\n## Testing\nMore text').entries, [{group: 'Improved', text: 'Improved scrolling.'}]);
});

test('malformed sections cannot silently lose a migration or publish icons', () => {
  for (const body of ['Release Notes:\n', 'Release Notes:\n\n- Deleted settings.', 'Release Notes:\n\nUnstructured text', 'Release Notes:\n\n- Added \u2728 an icon.']) {
    assert.throws(() => parseReleaseNotes(body), /Release Notes/);
  }
});

test('generation uses reachable application tags, excludes producer tags, and reports unannotated history', t => {
  const f = fixture(t);
  f.git('checkout', '-b', 'unrelated');
  f.commit('Unrelated release');
  f.git('tag', 'v9.0.0');
  f.git('checkout', 'main');
  f.commit('Legacy change without notes');
  f.git('tag', 'language-packs-2026.10.20.2');
  f.git('tag', 'lsp-servers-2026.10.20.1');
  const current = f.commit('A clear title without conventional prefixes\n\nRelease Notes:\n\n- Added selectable documentation.\n- Fixed labels.\n- Improved scrolling.\n- Upgrade: Rename the old setting.');
  f.git('tag', 'v0.1.3');
  const result = generateReleaseNotes({root: f.directory, version: '0.1.3'});
  assert.equal(result.previousTag, 'v0.1.2');
  assert.equal(result.missing.length, 1);
  assert.match(result.text, /## Added/);
  assert.match(result.text, /## Improved/);
  assert.match(result.text, /## Fixed/);
  assert.match(result.text, /## Upgrade notes/);
  assert.ok(result.text.includes(`/commit/${current}`));
  assert.match(result.text, /compare\/v0\.1\.2\.\.\.v0\.1\.3/);
  assert.ok(!result.text.includes('old feature'));
  assert.ok(!result.text.includes('Unrelated'));
  assert.ok(!result.text.includes('Legacy change'));
});

test('N/A is skipped, duplicate notes are folded, and current checkout bounds the generated history', t => {
  const f = fixture(t);
  const body = 'Release Notes:\n\n- Fixed labels.';
  f.commit('First fix\n\n' + body);
  const target = f.commit('Cherry-picked fix\n\n' + body);
  f.commit('Internal change\n\nRelease Notes:\n\n- N/A');
  f.commit('Later feature\n\nRelease Notes:\n\n- Added future text.');
  const result = generateReleaseNotes({root: f.directory, version: '0.1.3', ref: target});
  assert.equal(result.text.match(/Fixed labels/g).length, 1);
  assert.ok(!result.text.includes('future text'));
});

test('historical Changed and Removed notes survive generation with their commit links', t => {
  const f = fixture(t);
  const hash = f.commit('Preview and Changes polish\n\nRelease Notes:\n\n- Changed Markdown selection to match the editor,\n  without gaps between lines.\n- Removed group headings from the All Changes file list.');
  const result = generateReleaseNotes({root: f.directory, version: '0.1.3'});
  assert.equal(result.missing.length, 0);
  assert.match(result.text, /## Changed\n\n- Changed Markdown selection to match the editor, without gaps between lines\./);
  assert.match(result.text, /## Removed\n\n- Removed group headings from the All Changes file list\./);
  assert.equal(result.text.split('/commit/' + hash).length - 1, 2);
  assert.throws(() => parseReleaseNotes('Release Notes:\n\n- Deleted settings.'), /Unsupported Release Notes entry/);
});

test('first releases and releases without user-facing entries still have useful notes', t => {
  const f = fixture(t);
  f.git('tag', '-d', 'v0.1.2');
  let result = generateReleaseNotes({root: f.directory, version: '0.1.3'});
  assert.equal(result.previousTag, null);
  assert.match(result.text, /Added the old feature/);
  assert.match(result.text, /commits\/v0\.1\.3/);
  f.git('tag', 'v0.1.2');
  f.commit('Internal change\n\nRelease Notes:\n\n- N/A');
  result = generateReleaseNotes({root: f.directory, version: '0.1.3'});
  assert.match(result.text, /No user-facing changes were recorded/);
  assert.ok(!result.text.includes('## Added'));
  assert.throws(() => generateReleaseNotes({root: f.directory, version: '../../escape'}), /Invalid release version/);
});

test('draft preparation requires the existing tag to match the build and real assets without a version-only source commit', t => {
  const f = fixture(t);
  fs.writeFileSync(path.join(f.directory, 'Cargo.toml'), '[package]\nname = "loog"\nversion = "0.1.3"\n');
  const assets = path.join(f.directory, 'dist');
  fs.mkdirSync(assets);
  const notes = path.join(f.directory, 'target/release-notes.md');
  const options = {root: f.directory, version: '0.1.3', repo: 'test/loog', assets, notes};
  assert.throws(() => prepareDraft(options), /must already exist/);
  assert.equal(f.git('tag', '--list', 'v0.1.3'), '');
  f.git('tag', 'v0.1.3');
  assert.throws(() => prepareUpload(options), /No release assets/);
  fs.writeFileSync(path.join(assets, 'Loog.zip'), 'artifact');
  const prepared = prepareUpload(options);
  assert.equal(prepared.assets.length, 1);
  prepareDraft(options);
  assert.match(fs.readFileSync(notes, 'utf8'), /# Loog v0\.1\.3/);
  assert.throws(() => prepareDraft({...options, version: '0.1.4'}), /must already exist/);
  f.commit('Newer source\n\nRelease Notes:\n\n- N/A');
  assert.throws(() => prepareDraft(options), /does not point to the build commit/);
});

const ok = stdout => ({status: 0, stdout, stderr: ''});
const failed = stderr => ({status: 1, stdout: '', stderr});
const options = {version: '0.1.3', repo: 'test/loog', notes: '/tmp/release-notes.md', assets: ['/tmp/dist/Loog package.zip']};

test('the single publisher creates a draft with file notes and never creates a tag or publishes', async () => {
  const calls = [];
  const responses = [failed('not found'), ok('created'), ok('true'), ok('uploaded')];
  await createDraft(options, args => { calls.push(args); return responses.shift(); });
  assert.deepEqual(calls.map(args => args[1]), ['view', 'create', 'view']);
  assert.ok(calls[1].includes('--draft'));
  assert.ok(calls[1].includes('--verify-tag'));
  assert.equal(calls[1][calls[1].indexOf('--notes-file') + 1], options.notes);
  assert.ok(!calls.flat().includes('--generate-notes'));
  assert.ok(!calls.some(args => args[1] === 'edit'));
});

test('later platforms preserve the existing draft body and upload their own assets', async () => {
  const calls = [];
  await uploadToDraft(options, args => { calls.push(args); return ok(args[1] === 'view' ? 'true' : 'uploaded'); });
  assert.deepEqual(calls.map(args => args[1]), ['view', 'upload']);
  assert.ok(calls[1].includes('/tmp/dist/Loog package.zip'));
});

test('simultaneous manual draft creation is recovered only after verification', async () => {
  const calls = [];
  const responses = [failed('not found'), failed('already exists'), ok('true'), ok('uploaded')];
  await createDraft(options, args => { calls.push(args); return responses.shift(); });
  assert.deepEqual(calls.map(args => args[1]), ['view', 'create', 'view']);
});

test('published releases, API creation failures and upload failures remain failures', async () => {
  const publishedCalls = [];
  await assert.rejects(uploadToDraft(options, args => { publishedCalls.push(args); return ok('false'); }), /already published/);
  assert.equal(publishedCalls.length, 1);
  for (const [responses, expected] of [
    [[failed('not found'), failed('permission denied'), failed('not found')], /permission denied/],
    [[failed('not found'), ok('created'), failed('cannot verify')], /Could not verify/],
    [[failed('not found'), failed('already exists'), ok('false')], /already published/],
  ]) await assert.rejects(createDraft(options, () => responses.shift()), expected);
  const interrupted = [ok('true'), failed('upload interrupted')];
  await assert.rejects(uploadToDraft(options, () => interrupted.shift()), /upload interrupted/);
  await assert.rejects(uploadToDraft(options, () => failed('not found'), {timeoutMs:0}), /publish-draft/);
});
