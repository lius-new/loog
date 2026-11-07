import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawn, spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {startupSmoke, normalizeWindowsPath, buildCloseHelper} from './startup-smoke.mjs';

test('session path comparison supports verbatim, UNC, case and separator differences', () => {
  assert.equal(normalizeWindowsPath('\\\\?\\C:\\工作区\\'), normalizeWindowsPath('c:/工作区'));
  assert.equal(normalizeWindowsPath('\\\\?\\UNC\\server\\share\\folder'), normalizeWindowsPath('\\\\SERVER\\share\\folder'));
});

test('Windows GUI smoke requests normal close and diagnoses startup, session and timeout failures', {skip: process.platform !== 'win32', timeout: 60000}, async t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'loog-startup-中文 '));
  t.after(() => fs.rmSync(root, {recursive: true, force: true, maxRetries: 5, retryDelay: 100}));
  const executable = path.join(root, 'startup probe.exe');
  const built = spawnSync('rustc', ['--edition=2024', '--crate-name=loog_startup_fixture',
    fileURLToPath(new URL('./fixtures/startup-probe.rs', import.meta.url)), '-o', executable], {encoding: 'utf8', windowsHide: true, timeout: 30000});
  assert.equal(built.status, 0, built.error?.message || built.stderr);
  for (const mode of ['normal', 'early-exit', 'no-session', 'wrong-session', 'bad-exit', 'ignore-close']) {
    await t.test(mode, async () => {
      const outputDirectory = path.join(root, mode);
      const result = await startupSmoke({executable, outputDirectory, startupMs: 300, closeTimeoutMs: mode === 'ignore-close' ? 300 : 3000,
        env: {...process.env, LOOG_STARTUP_FIXTURE_MODE: mode}});
      assert.equal(result.result, mode === 'normal' ? 'passed' : 'failed', JSON.stringify(result));
      assert.equal(result.forced_cleanup, mode === 'ignore-close', JSON.stringify(result));
      if (mode === 'normal') {assert.equal(result.close_requested, true); assert.equal(result.saved_synthetic_workspace, true);}
      if (mode === 'early-exit') assert.match(result.error, /during startup/);
      if (mode === 'no-session') assert.match(result.error, /did not save/);
      if (mode === 'wrong-session') assert.match(result.error, /synthetic workspace/);
      if (mode === 'bad-exit') assert.match(result.error, /code 7/);
      if (mode === 'ignore-close') assert.match(result.error, /did not close normally/);
      assert.deepEqual(JSON.parse(fs.readFileSync(path.join(outputDirectory, 'startup-smoke.json'))), result);
      if (result.pid) assert.throws(() => process.kill(result.pid, 0));
    });
  }
  for (const [name, startDelay, closeDelay] of [['delayed-window', 1200, 0], ['delayed-close', 0, 1200]]) {
    await t.test(name, async () => {
      const result = await startupSmoke({executable, outputDirectory:path.join(root, name), startupMs:100,
        startupTimeoutMs:10000, closeTimeoutMs:10000, env:{...process.env, LOOG_STARTUP_FIXTURE_MODE:'normal',
          LOOG_STARTUP_FIXTURE_START_DELAY_MS:String(startDelay), LOOG_STARTUP_FIXTURE_CLOSE_DELAY_MS:String(closeDelay)}});
      assert.equal(result.result, 'passed', JSON.stringify(result));
      assert.equal(result.forced_cleanup, false);
      assert.equal(result.saved_synthetic_workspace, true);
    });
  }
  await t.test('a window that never becomes ready remains a failure and its child is cleaned up', async () => {
    const result = await startupSmoke({executable, outputDirectory:path.join(root, 'no-window'), startupMs:50,
      startupTimeoutMs:500, env:{...process.env, LOOG_STARTUP_FIXTURE_START_DELAY_MS:'60000'}});
    assert.equal(result.result, 'failed');
    assert.match(result.error, /Timed out waiting.*main window/);
    assert.equal(result.forced_cleanup, true);
    assert.throws(() => process.kill(result.pid, 0));
  });
  await t.test('cancellation during observation cleans up only the created child', async () => {
    const controller = new AbortController();
    const timer = setTimeout(() => controller.abort(), 600);
    try {
      const result = await startupSmoke({executable, outputDirectory: path.join(root, 'cancelled'),
        startupMs: 4000, signal: controller.signal, env: {...process.env, LOOG_STARTUP_FIXTURE_MODE: 'normal'}});
      assert.equal(result.result, 'failed');
      assert.match(result.error, /cancelled/);
      if (result.pid) {assert.equal(result.forced_cleanup, true); assert.throws(() => process.kill(result.pid, 0));}
    } finally {clearTimeout(timer);}
  });
  await t.test('helper refuses a mismatched executable and never closes the unrelated process', async () => {
    const helper = buildCloseHelper(root);
    const child = spawn(executable, [root], {env: {...process.env, APPDATA: root}, windowsHide: true, stdio: 'ignore'});
    const finished = new Promise(resolve => child.once('close', resolve));
    try {
      await new Promise(resolve => child.once('spawn', resolve));
      const result = spawnSync(helper, [String(child.pid), process.execPath], {encoding: 'utf8', windowsHide: true});
      assert.equal(result.status, 1);
      assert.match(result.stderr, /does not match/);
      assert.doesNotThrow(() => process.kill(child.pid, 0));
    } finally {child.kill(); await finished;}
  });
});
