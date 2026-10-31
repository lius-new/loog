// Type-checks, bundles and componentizes a TypeScript extension.
//
//   node sdk/typescript/build.mjs [entry.ts] [out.wasm]
//
// Defaults build the reference extension to target/extension-guest/ts_guest.wasm.
// The toolchain is the pinned one of tests/extensions/js-guest.
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = join(here, '..', '..');
const tools = join(root, 'tests', 'extensions', 'js-guest', 'node_modules');
const entry = resolve(process.argv[2] ?? join(here, 'example', 'src', 'main.ts'));
const out = resolve(process.argv[3] ?? join(root, 'target', 'extension-guest', 'ts_guest.wasm'));
const bundle = join(root, 'target', 'extension-guest', 'ts_guest.js');

function run(label, command, args) {
  const result = spawnSync(command, args, { cwd: root, stdio: 'inherit' });
  if (result.status !== 0) {
    console.error(`${label} failed`);
    process.exit(result.status ?? 1);
  }
}

if (!existsSync(join(tools, '@bytecodealliance', 'jco', 'dist', 'jco.js'))) {
  const npm = process.platform === 'win32' ? 'npm.cmd' : 'npm';
  const installed = spawnSync(npm, ['install', '--no-audit', '--no-fund'], {
    cwd: join(root, 'tests', 'extensions', 'js-guest'),
    stdio: 'inherit',
    shell: process.platform === 'win32',
  });
  if (installed.status !== 0) process.exit(installed.status ?? 1);
}

// 1. The types come from the contract itself.
run('generating types', process.execPath, [join(here, 'generate.mjs'), '--check']);

// 2. The sources must type-check as they are written.
const tsc = join(tools, 'typescript', 'bin', 'tsc');
run('type check', process.execPath, [existsSync(tsc) ? tsc : join(tools, 'typescript', 'lib', 'tsc.js'), '-p', join(here, 'tsconfig.json')]);

// 3. One file: the SDK and the extension, with the host interface left to the component.
mkdirSync(dirname(out), { recursive: true });
const { rolldown } = await import(pathToFileURL(join(tools, 'rolldown', 'dist', 'index.mjs')).href);
const built = await rolldown({
  input: entry,
  external: ['loom:extension/host@0.1.0'],
  resolve: { alias: { '@loom/extension': join(here, 'src', 'index.ts') } },
});
await built.write({ file: bundle, format: 'esm' });

// 4. The component.
run('componentize', process.execPath, [
  join(tools, '@bytecodealliance', 'jco', 'dist', 'jco.js'),
  'componentize', bundle,
  '--wit', join(root, 'wit', 'loom-extension'),
  '--world-name', 'extension',
  '--disable', 'stdio', 'random', 'clocks', 'http',
  '-o', out,
]);
console.log(`built ${out}`);
