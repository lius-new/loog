// Exercise the production installer's automatic update relaunch with a tiny
// GUI probe, without replacing Loog or registering a test installation.
import assert from 'node:assert/strict';
import { randomUUID } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import { setTimeout as delay } from 'node:timers/promises';
import { fileURLToPath } from 'node:url';
import { ROOT, TOOLS_DIR, WORK_DIR, run } from '../lib/util.mjs';

assert.equal(process.platform, 'win32', 'This smoke test requires Windows.');
const here = path.dirname(fileURLToPath(import.meta.url));
const compiler = process.argv[2] || path.join(TOOLS_DIR, 'innosetup-7.1.0', 'ISCC.exe');
assert.ok(fs.existsSync(compiler), 'Run the Windows packaging setup step first.');
fs.mkdirSync(WORK_DIR, { recursive: true });
const scratch = fs.mkdtempSync(path.join(WORK_DIR, 'windows-launch-smoke-'));
const stage = path.join(scratch, 'stage');
const install = path.join(scratch, 'installed app');
const version = path.join(scratch, 'toolchain', '1.0');
const current = path.join(scratch, 'toolchain', 'current');
const report = path.join(scratch, 'launch-report.txt');
let passed = false;

try {
  fs.mkdirSync(stage);
  fs.mkdirSync(version, { recursive: true });
  fs.writeFileSync(path.join(version, 'npm.cmd'), '@echo off\r\necho loog-junction-probe\r\n');
  // Use Scoop's current-junction structure without requesting elevation.
  fs.symlinkSync(version, current, 'junction');
  assert.ok(fs.existsSync(path.join(current, 'npm.cmd')), 'The test junction must be accessible.');
  run('rustc', [
    '--edition=2024', '--crate-name=loog_installer_launch_probe',
    path.join(here, 'launch-probe.rs'), '-o', path.join(stage, 'Loog.exe'),
  ], {
    env: {
      ...process.env,
      LOOG_INSTALLER_PROBE_TOOLCHAIN: current,
      LOOG_INSTALLER_PROBE_REPORT: report,
    },
  });

  const original = fs.readFileSync(path.join(here, 'installer.iss'), 'utf8');
  // Keep the production setup policy and [Run]/[Code] paths. Isolate the app
  // identity and omit shell integration so the real installation is untouched.
  const fixture = original
    .replace('#define AppName "Loog"', '#define AppName "Loog Launch Smoke"')
    .replace(/^AppId=.*$/m, `AppId={{${randomUUID().toUpperCase()}}`)
    .replace('[Setup]', '[Setup]\nUninstallable=no\nCreateUninstallRegKey=no')
    .replace(/\[Icons\][\s\S]*?(?=\[Run\])/, '');
  assert.notEqual(fixture, original);
  const script = path.join(scratch, 'installer.iss');
  fs.writeFileSync(script, fixture);
  run(compiler, [
    '/Qp', '/DVersion=0.0.0', '/DVersionQuad=0.0.0.0',
    `/DStageDir=${stage}`, `/DOutputDir=${scratch}`, '/DOutputBase=launch-smoke-setup',
    `/DAppIcon=${path.join(ROOT, 'icons', 'app.ico')}`, '/DHomepage=https://example.invalid', script,
  ]);
  run(path.join(scratch, 'launch-smoke-setup.exe'), [
    '/VERYSILENT', '/SUPPRESSMSGBOXES', '/NORESTART', '/SP-', '/TASKS=',
    `/DIR=${install}`, `/LOG=${path.join(scratch, 'setup.log')}`, '/relaunch=1',
  ]);
  const deadline = Date.now() + 30_000;
  while (!fs.existsSync(report) && Date.now() < deadline) await delay(100);
  assert.ok(fs.existsSync(report), 'The installer did not automatically launch the probe.');
  const result = fs.readFileSync(report, 'utf8');
  console.log(result);
  assert.match(result, /^query=1$/m, 'The process policy query must succeed.');
  assert.match(result, /^redirection_guard=0$/m, 'The launched app must not inherit RedirectionGuard.');
  for (const key of ['junction_visible', 'cmd_npm', 'powershell_npm']) {
    assert.match(result, new RegExp(`^${key}=true$`, 'm'), `${key} failed after installer relaunch.`);
  }
  const log = fs.readFileSync(path.join(scratch, 'setup.log'), 'utf8');
  assert.match(log, /RedirectionGuard status for current process: Not enabling/i);
  console.log('Installer relaunch preserves junction access in the app, CMD and PowerShell.');
  passed = true;
} finally {
  if (passed) {
    // Verify the absolute target before recursively removing our own fixture.
    const relative = path.relative(path.resolve(WORK_DIR), path.resolve(scratch));
    assert.ok(relative.startsWith('windows-launch-smoke-') && !relative.includes(path.sep));
    // Remove the junction first; never recursively walk its target during cleanup.
    fs.unlinkSync(current);
    fs.rmSync(scratch, { recursive: true, force: true });
  } else {
    console.error(`Smoke-test files retained for inspection: ${scratch}`);
  }
}
