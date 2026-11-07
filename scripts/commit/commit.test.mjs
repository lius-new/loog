import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {checkMessage, checkEvent} from './check.mjs';
import {install} from './install.mjs';

const root = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const internal = 'chore: internal change\n\nRelease Notes:\n- N/A\n';
const feature = 'fix: user-facing change\n\nRelease Notes:\n- Fixed opening files\n  with spaces in their names.\n\nCo-authored-by: Test <test@example.com>\n';
const git = (cwd, args) => execFileSync('git', args, {cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'], env: {...process.env, GIT_CONFIG_GLOBAL: path.join(cwd, 'empty-global-config'), GIT_CONFIG_NOSYSTEM: '1'}}).trim();
function repository(t) {
  const cwd = fs.realpathSync(fs.mkdtempSync(path.join(os.tmpdir(), 'loog-commit-')));
  t.after(() => fs.rmSync(cwd, {recursive: true, force: true}));
  git(cwd, ['init', '-q']);
  git(cwd, ['config', 'user.name', 'Commit Test']);
  git(cwd, ['config', 'user.email', 'commit-test@example.com']);
  git(cwd, ['config', 'commit.gpgsign', 'false']);
  // Keep installer calls independent of the developer's global configuration too.
  git(cwd, ['config', 'core.hooksPath', path.join(cwd, '.githooks')]);
  git(cwd, ['config', 'commit.template', path.join(cwd, '.github/commit-message.txt')]);
  for (const file of ['.githooks/commit-msg', '.github/commit-message.txt', 'scripts/commit/check.mjs', 'scripts/release/notes.mjs']) {
    const destination = path.join(cwd, file);
    fs.mkdirSync(path.dirname(destination), {recursive: true});
    fs.copyFileSync(path.join(root, file), destination);
  }
  return cwd;
}
function commit(cwd, message, {bypass = false, amend = false} = {}) {
  git(cwd, [...(bypass ? ['-c', 'core.hooksPath=empty-hooks'] : []), 'commit', '--allow-empty', ...(amend ? ['--amend'] : []), '-m', message]);
  return git(cwd, ['rev-parse', 'HEAD']);
}

test('internal notes, real notes, wrapping, trailers and CRLF share the release parser', () => {
  assert.deepEqual(checkMessage(internal), {present: true, entries: []});
  const parsed = checkMessage(feature.replaceAll('\n', '\r\n'));
  assert.deepEqual(parsed.entries, [{group: 'Fixed', text: 'Fixed opening files with spaces in their names.'}]);
  assert.equal(checkMessage('feat: migrate\n\nRelease Notes:\n- Upgrade: Rebuild the cache.').entries[0].group, 'Upgrade notes');
});
test('missing, empty, placeholder and malformed notes are rejected', () => {
  for (const body of ['chore: only a title', 'fix: a change\n\nRelease Notes:',
    'fix: a change\n\nRelease Notes:\n- <note or N/A>', 'fix: a change\n\nRelease Notes:\nN/A',
    'fix: a change\n\nRelease Notes:\n- Something changed', 'fix: a change\n\nRelease Notes:\n- Fixed 🚀 something',
    'fix: a change\n\nRelease Notes\n- N/A', 'fix: a change\n\n## Release Notes:\n- N/A']) {
    assert.throws(() => checkMessage(body), Error, body);
  }
});
test('duplicate sections, conflicting N/A and headings only in fenced examples fail', () => {
  for (const body of [internal + '\nRelease Notes:\n- N/A', internal + '\n- Fixed a bug.', internal + '- N/A',
    'example\n\x60\x60\x60text\nRelease Notes:\n- N/A\n\x60\x60\x60',
    'example\n~~~text\nRelease Notes:\n- N/A\n~~~']) assert.throws(() => checkMessage(body));
  assert.doesNotThrow(() => checkMessage('example\n\x60\x60\x60text\nRelease Notes:\n- N/A\n\x60\x60\x60\n\n' + feature));
});
test('real Git hook blocks commit/amend and accepts explicit N/A and user notes', t => {
  const cwd = repository(t);
  install(cwd);
  assert.equal(install(cwd), cwd.replaceAll('\\', '/'));
  assert.throws(() => commit(cwd, 'chore: missing notes'), /Release Notes/);
  assert.throws(() => git(cwd, ['rev-parse', '--verify', 'HEAD']));
  const first = commit(cwd, internal);
  assert.throws(() => commit(cwd, 'fix: missing notes', {amend: true}), /Release Notes/);
  assert.equal(git(cwd, ['rev-parse', 'HEAD']), first);
  assert.throws(() => commit(cwd, fs.readFileSync(path.join(cwd, '.github/commit-message.txt'), 'utf8')), /Release Notes/);
  const next = commit(cwd, feature);
  assert.notEqual(first, next);
  assert.equal(git(cwd, ['log', '-1', '--format=%B']).includes('Release Notes:'), true);
  assert.equal(git(cwd, ['config', '--local', 'core.hooksPath']).replaceAll('\\', '/'), path.join(cwd, '.githooks').replaceAll('\\', '/'));
});
test('installer preserves custom hook/template config without partial changes', t => {
  const cwd = repository(t);
  git(cwd, ['config', 'core.hooksPath', 'custom-hooks']);
  assert.throws(() => install(cwd), /Existing core.hooksPath/);
  assert.equal(git(cwd, ['config', 'core.hooksPath']), 'custom-hooks');
  git(cwd, ['config', 'core.hooksPath', path.join(cwd, '.githooks')]);
  git(cwd, ['config', 'commit.template', 'custom-template']);
  assert.throws(() => install(cwd), /Existing commit.template/);
  assert.equal(git(cwd, ['config', 'commit.template']), 'custom-template');
});
test('installer accepts hook and template paths through a repository directory alias', t => {
  const cwd = repository(t);
  const alias = path.join(cwd, 'alias');
  fs.symlinkSync(cwd, alias, process.platform === 'win32' ? 'junction' : 'dir');
  git(cwd, ['config', 'core.hooksPath', path.join(alias, '.githooks')]);
  git(cwd, ['config', 'commit.template', path.join(alias, '.github/commit-message.txt')]);
  assert.equal(install(alias), git(cwd, ['rev-parse', '--show-toplevel']));
  assert.equal(install(cwd), git(cwd, ['rev-parse', '--show-toplevel']));
  assert.throws(() => commit(cwd, 'chore: missing notes'), /Release Notes/);
  commit(cwd, internal);
});
test('installer does not hide an existing default commit-msg hook', t => {
  const cwd = repository(t);
  git(cwd, ['config', '--unset', 'core.hooksPath']);
  fs.writeFileSync(path.join(cwd, '.git/hooks/commit-msg'), '#!/bin/sh\nexit 0\n');
  assert.throws(() => install(cwd), /Existing .git\/hooks\/commit-msg/);
  assert.throws(() => git(cwd, ['config', '--local', '--get', 'core.hooksPath']));
});
test('CI checks every new commit even when a local hook was bypassed and tip is valid', t => {
  const cwd = repository(t);
  const base = commit(cwd, 'legacy missing notes', {bypass: true});
  const bad = commit(cwd, 'new missing notes', {bypass: true});
  const head = commit(cwd, internal, {bypass: true});
  for (const eventName of ['push', 'pull_request']) {
    const event = eventName === 'push' ? {before: base, after: head} : {pull_request: {base: {sha: base}, head: {sha: head}}};
    assert.throws(() => checkEvent({eventName, event, cwd}), error => error.message.includes(bad.slice(0, 12)) && !error.message.includes(base.slice(0, 12)));
  }
});
test('PR checks its real head, excludes legacy base, and does not check synthetic merge HEAD', t => {
  const cwd = repository(t);
  const base = commit(cwd, 'legacy missing notes', {bypass: true});
  const first = commit(cwd, internal, {bypass: true});
  const head = commit(cwd, feature, {bypass: true});
  commit(cwd, 'GitHub synthetic merge without notes', {bypass: true});
  assert.equal(checkEvent({eventName: 'pull_request', event: {pull_request: {base: {sha: base}, head: {sha: head}}}, cwd}), 2);
  assert.equal(checkEvent({eventName: 'push', event: {before: first, after: head}, cwd}), 1);
  assert.throws(() => checkEvent({eventName: 'workflow_dispatch', event: {}, cwd}), /Release Notes/);
});
test('manual/new-branch checks validate tip, while missing history/events fail closed', t => {
  const cwd = repository(t);
  const base = commit(cwd, 'legacy missing notes', {bypass: true});
  const head = commit(cwd, internal, {bypass: true});
  assert.equal(checkEvent({eventName: 'workflow_dispatch', event: {}, cwd}), 1);
  assert.equal(checkEvent({eventName: 'push', event: {before: '0'.repeat(40), after: head}, cwd}), 1);
  for (const event of [{before: 'f'.repeat(40), after: head}, {after: head}, {before: base, after: '--all'}, {before: head, after: head}]) {
    assert.throws(() => checkEvent({eventName: 'push', event, cwd}));
  }
  assert.throws(() => checkEvent({eventName: 'unknown', event: {}, cwd}), /Unsupported/);
});

test('CI command consumes the event file and exits nonzero for missing notes', t => {
  const cwd = repository(t);
  const base = commit(cwd, internal, {bypass: true});
  const bad = commit(cwd, 'missing notes', {bypass: true});
  const eventFile = path.join(cwd, 'event.json');
  const run = () => execFileSync(process.execPath, ['scripts/commit/check.mjs', '--event'], {
    cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe'],
    env: {...process.env, GITHUB_EVENT_NAME: 'push', GITHUB_EVENT_PATH: eventFile},
  });
  fs.writeFileSync(eventFile, JSON.stringify({before: base, after: bad}));
  assert.throws(run, /Release Notes/);
  const head = commit(cwd, feature, {bypass: true});
  fs.writeFileSync(eventFile, JSON.stringify({before: bad, after: head}));
  assert.match(run(), /validated for 1 commit/);
});
test('installer preserves other active default hooks', t => {
  const cwd = repository(t);
  git(cwd, ['config', '--unset', 'core.hooksPath']);
  fs.writeFileSync(path.join(cwd, '.git/hooks/pre-commit'), '#!/bin/sh\nexit 0\n');
  assert.throws(() => install(cwd), /Existing .git\/hooks\/pre-commit/);
});
