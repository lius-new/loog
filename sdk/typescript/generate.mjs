// Generates the TypeScript types of the host contract into sdk/typescript/generated.
//
//   node sdk/typescript/generate.mjs           write the files
//   node sdk/typescript/generate.mjs --check   fail if the files are out of date
//
// The types come from wit/loom-extension/world.wit through the pinned jco
// (tests/extensions/js-guest/package.json), so they cannot drift from the contract.
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, readdirSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { dirname, join, relative } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..', '..');
const jco = join(root, 'tests', 'extensions', 'js-guest', 'node_modules', '@bytecodealliance', 'jco', 'dist', 'jco.js');
const target = join(here, 'generated');
const check = process.argv.includes('--check');

if (!existsSync(jco)) {
  const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';
  const installed = spawnSync(npm, ['install', '--no-audit', '--no-fund'], {
    cwd: join(root, 'tests', 'extensions', 'js-guest'),
    stdio: 'inherit',
    shell: process.platform === 'win32',
  });
  if (installed.status !== 0) process.exit(installed.status ?? 1);
}

function files(directory) {
  const found = new Map();
  if (!existsSync(directory)) return found;
  const walk = (current) => {
    for (const name of readdirSync(current)) {
      const path = join(current, name);
      if (statSync(path).isDirectory()) walk(path);
      else found.set(relative(directory, path).replaceAll('\\', '/'), readFileSync(path, 'utf8').replaceAll('\r\n', '\n'));
    }
  };
  walk(directory);
  return found;
}

const output = check ? mkdtempSync(join(tmpdir(), 'loom-sdk-types-')) : target;
if (!check) {
  rmSync(target, { recursive: true, force: true });
  mkdirSync(target, { recursive: true });
}
const result = spawnSync(
  process.execPath,
  [jco, 'types', join(root, 'wit', 'loom-extension'), '--world-name', 'extension', '-o', output],
  { stdio: check ? 'ignore' : 'inherit' },
);
if (result.status !== 0) {
  console.error('jco types failed');
  process.exit(result.status ?? 1);
}

// jco emits whitespace on empty documentation lines. Keep generated sources
// deterministic and compatible with the repository's whitespace checks.
for (const [name, source] of files(output)) {
  writeFileSync(join(output, name), source.replace(/[\t ]+$/gm, ''));
}

if (check) {
  const fresh = files(output);
  const current = files(target);
  rmSync(output, { recursive: true, force: true });
  const stale = [...new Set([...fresh.keys(), ...current.keys()])].filter((name) => fresh.get(name) !== current.get(name));
  if (stale.length > 0) {
    console.error(`sdk/typescript/generated is out of date (${stale.join(', ')}); run node sdk/typescript/generate.mjs`);
    process.exit(1);
  }
  console.log('sdk/typescript/generated is up to date');
}
