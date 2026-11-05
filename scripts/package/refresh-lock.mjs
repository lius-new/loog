// Lock the bundled extensions against the registry's live signed index.
//
//   node scripts/package/refresh-lock.mjs [--bundle id,id,...]
//
// Rewrites the generated files in assets/registry (see lib/refresh.mjs). Packaging
// does this on its own when they are missing; run it to pick up newer extensions or
// to see what a release would bundle. The release workflow runs it once and hands the
// result to every platform build.
import {BUNDLED} from './lib/lock.mjs';
import {refreshRegistry} from './lib/refresh.mjs';

try {
  const args = process.argv.slice(2);
  if (args.length && !(args.length === 2 && args[0] === '--bundle')) throw new Error('Usage: node scripts/package/refresh-lock.mjs [--bundle id,id,...]');
  const lock = await refreshRegistry({ids: args[1]?.split(',') ?? BUNDLED});
  console.log(`Locked ${lock.extensions.map(e => `${e.id} ${e.version}`).join(', ')} against index ${lock.index.sequence}`);
} catch (error) {
  console.error(error.message);
  process.exitCode = 1;
}
