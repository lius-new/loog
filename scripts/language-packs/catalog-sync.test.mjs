import test from 'node:test';
import {checkMessage} from '../commit/check.mjs';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import path from 'node:path';
import {execFileSync, spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';

const root = fileURLToPath(new URL('../../', import.meta.url));
const catalog = 'assets/extensions/registry.toml';
const workflow = fs.readFileSync(path.join(root, '.github/workflows/language-packs.yml'), 'utf8').replaceAll('\r\n', '\n');
const step = workflow.split('      - name: Commit and push the released catalog\n')[1].split('\n      - ')[0];
const script = step.split('        run: |\n')[1].split('\n').map(line => line.slice(10)).join('\n');
const bash = process.platform === 'win32'
  ? path.resolve(execFileSync('git', ['--exec-path'], {encoding: 'utf8'}).trim(), '../../../bin/bash.exe')
  : 'bash';

function git(cwd, ...args) {
  return execFileSync('git', ['-c', 'user.name=Test', '-c', 'user.email=test@example.invalid',
    '-c', 'commit.gpgsign=false', '-c', 'core.autocrlf=false', ...args], {cwd, encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe']}).trim();
}

function fixture(t) {
  const parent = path.join(root, '.packaging/language-packs/sync-tests');
  fs.mkdirSync(parent, {recursive: true});
  const work = fs.mkdtempSync(path.join(parent, 'case-'));
  t.after(() => {
    assert.ok(path.resolve(work).startsWith(path.resolve(parent) + path.sep));
    fs.rmSync(work, {recursive: true, force: true});
  });
  const remote = path.join(work, 'remote.git'), runner = path.join(work, 'runner'), writer = path.join(work, 'writer');
  git(work, 'init', '--bare', remote);
  git(work, 'clone', remote, writer);
  git(writer, 'checkout', '-b', 'main');
  fs.mkdirSync(path.join(writer, path.dirname(catalog)), {recursive: true});
  fs.writeFileSync(path.join(writer, catalog), 'base\n');
  git(writer, 'add', '.'); git(writer, 'commit', '-m', 'Initial catalog'); git(writer, 'push', 'origin', 'main');
  git(work, 'clone', '--depth', '1', '--no-local', '--branch', 'main', remote, runner);
  git(runner, 'checkout', '--detach');
  fs.writeFileSync(path.join(runner, catalog), 'published batch\n');
  const summary = path.join(work, 'summary');
  const sync = () => spawnSync(bash, ['--noprofile', '--norc', '-e', '-o', 'pipefail', '-c', script], {
    cwd: runner, encoding: 'utf8', env: {...process.env, CATALOG_BRANCH: 'main', RESOURCE_TAG: 'language-packs-test',
      GITHUB_STEP_SUMMARY: summary.replaceAll('\\', '/'), GIT_CONFIG_COUNT: '1', GIT_CONFIG_KEY_0: 'commit.gpgsign', GIT_CONFIG_VALUE_0: 'false'},
  });
  return {remote, runner, writer, summary, sync};
}

function succeeded(result) {
  assert.equal(result.status, 0, result.error?.message || result.stderr || result.stdout);
}

test('the workflow pushes only the released catalog and an unchanged rerun creates no commit', t => {
  const f = fixture(t);
  assert.equal(git(f.runner, 'rev-parse', '--is-shallow-repository'), 'true');
  const producer = git(f.runner, 'rev-parse', 'HEAD');
  fs.writeFileSync(path.join(f.runner, 'untracked'), 'leave this local\n');
  succeeded(f.sync());
  const tip = git(f.remote, 'rev-parse', 'refs/heads/main');
  assert.deepEqual(checkMessage(git(f.remote, 'log', '-1', '--format=%B', tip)).entries, [{group: 'Changed', text: 'Changed the bundled language package catalog.'}]);
  assert.equal(git(f.remote, 'show', 'main:' + catalog), 'published batch');
  assert.equal(git(f.remote, 'diff-tree', '--no-commit-id', '--name-only', '-r', tip), catalog);
  assert.match(fs.readFileSync(f.summary, 'utf8'), /Committed and pushed/);
  succeeded(f.sync());
  assert.equal(git(f.remote, 'rev-parse', 'refs/heads/main'), tip);
  assert.match(fs.readFileSync(f.summary, 'utf8'), /already committed/);
  // An Actions rerun starts from the original producer commit again.
  git(f.runner, 'checkout', '--detach', producer);
  fs.writeFileSync(path.join(f.runner, catalog), 'published batch\n');
  succeeded(f.sync());
  assert.equal(git(f.remote, 'rev-parse', 'refs/heads/main'), tip);
});

test('the workflow preserves unrelated commits pushed while language packages were building', t => {
  const f = fixture(t);
  fs.writeFileSync(path.join(f.writer, 'other.txt'), 'concurrent work\n');
  git(f.writer, 'add', '.'); git(f.writer, 'commit', '-m', 'Unrelated work'); git(f.writer, 'push', 'origin', 'main');
  const previous = git(f.remote, 'rev-parse', 'refs/heads/main');
  succeeded(f.sync());
  assert.equal(git(f.remote, 'rev-parse', 'main^'), previous);
  assert.equal(git(f.remote, 'show', 'main:other.txt'), 'concurrent work');
  assert.equal(git(f.remote, 'show', 'main:' + catalog), 'published batch');
});

test('a conflicting catalog fails without overwriting the remote branch', t => {
  const f = fixture(t);
  fs.writeFileSync(path.join(f.writer, catalog), 'another published batch\n');
  git(f.writer, 'add', '.'); git(f.writer, 'commit', '-m', 'Another catalog'); git(f.writer, 'push', 'origin', 'main');
  const previous = git(f.remote, 'rev-parse', 'refs/heads/main');
  const result = f.sync();
  assert.notEqual(result.status, 0);
  assert.match(result.stdout + result.stderr, /CONFLICT/);
  assert.equal(git(f.remote, 'rev-parse', 'refs/heads/main'), previous);
  assert.equal(git(f.remote, 'show', 'main:' + catalog), 'another published batch');
});

test('a rejected push fails the job and retains the generated catalog locally', t => {
  const f = fixture(t), previous = git(f.remote, 'rev-parse', 'refs/heads/main');
  fs.writeFileSync(path.join(f.remote, 'hooks/pre-receive'), '#!/bin/sh\nexit 1\n', {mode: 0o755});
  const result = f.sync();
  assert.notEqual(result.status, 0);
  assert.match(result.stderr, /hook declined|rejected/);
  assert.equal(git(f.remote, 'rev-parse', 'refs/heads/main'), previous);
  assert.equal(fs.readFileSync(path.join(f.runner, catalog), 'utf8'), 'published batch\n');
});
