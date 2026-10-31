// Repeatable extension test entry (EXTENSIONS_DESIGN.md M1-10).
//
//   node scripts/extensions/test.mjs          # API rules, host, Rust and JS guests, app integration
//   node scripts/extensions/test.mjs --quick  # skip the JavaScript guest (needs npm and ~15 s)
//
// The guests are built on demand: the Rust guest with cargo for
// wasm32-unknown-unknown, the JavaScript guest with the pinned jco.
import { spawnSync } from 'node:child_process';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const root = join(dirname(fileURLToPath(import.meta.url)), '..', '..');
const quick = process.argv.includes('--quick');

function run(label, command, args) {
  console.log(`\n== ${label}`);
  const result = spawnSync(command, args, { cwd: root, stdio: 'inherit' });
  if (result.status !== 0) {
    console.error(`\n${label} failed`);
    process.exit(result.status ?? 1);
  }
}

run('API rules', 'cargo', ['test', '--locked', '-p', 'loom-extension-api']);
run('host: Rust guest and compatibility', 'cargo', [
  'test', '--locked', '--release', '-p', 'loom-extension-host',
  '--test', 'rust_guest', '--test', 'compatibility',
]);
if (!quick) {
  run('host: JavaScript guest', 'cargo', ['test', '--locked', '--release', '-p', 'loom-extension-host', '--test', 'js_guest']);
  run('TypeScript SDK: types follow the contract', process.execPath, ['sdk/typescript/generate.mjs', '--check']);
  run('TypeScript SDK: unit tests', process.execPath, ['--test', 'sdk/typescript/test/sdk.test.ts']);
  run('host: TypeScript guest', 'cargo', ['test', '--locked', '--release', '-p', 'loom-extension-host', '--test', 'ts_guest']);
}
run('application integration', 'cargo', [
  'test', '--locked', '--bin', 'Loom', '--',
  'app::extensions', 'app::workspace_edits', 'features::extensions', 'infrastructure::extensions',
]);
