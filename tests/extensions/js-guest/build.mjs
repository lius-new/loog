// Builds guest.js into target/extension-guest/js_guest.wasm with the pinned jco.
// Usage: node tests/extensions/js-guest/build.mjs
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..', '..', '..');
const out = join(root, 'target', 'extension-guest', 'js_guest.wasm');
const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';

function run(command, args, cwd) {
  const result = spawnSync(command, args, { cwd, stdio: 'inherit', shell: process.platform === 'win32' && command === npm });
  if (result.status !== 0) {
    console.error(`${command} ${args.join(' ')} failed`);
    process.exit(result.status ?? 1);
  }
}

if (!existsSync(join(here, 'node_modules', '@bytecodealliance', 'jco', 'dist', 'jco.js'))) {
  run(npm, ['install', '--no-audit', '--no-fund'], here);
}
mkdirSync(dirname(out), { recursive: true });
run(process.execPath, [
  join(here, 'node_modules', '@bytecodealliance', 'jco', 'dist', 'jco.js'),
  'componentize', join(here, 'guest.js'),
  '--wit', join(root, 'wit', 'loog-extension'),
  '--world-name', 'extension',
  '--disable', 'stdio', 'random', 'clocks', 'http',
  '-o', out,
], root);
