import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {spawnSync} from 'node:child_process';
import {prepareDependencies, ROOT} from '../dependencies/ensure.mjs';

export function requirements(args, env = process.env) {
  const option = name => args.find(arg => arg.startsWith(name + '='))?.slice(name.length + 1) ?? (args.includes(name) ? args[args.indexOf(name) + 1] : undefined);
  const command = args[0];
  if (['fmt', 'metadata', 'tree', 'clean', '--version', '-V', 'help'].includes(command) || args.includes('--help') || args.includes('-V')) return null;
  const packages = args.flatMap((arg, i) => arg === '-p' || arg === '--package' ? [args[i + 1]] : arg.startsWith('--package=') ? [arg.slice(10)] : []);
  if (!args.includes('--workspace') && packages.length && packages.every(p => ['loog-editor-core', 'loog-workspace-tests'].includes(p))) return null;
  return {target: args.some(arg => arg === '--target' || arg.startsWith('--target=')) ? option('--target') : env.CARGO_BUILD_TARGET, offline: args.includes('--offline') || args.includes('--frozen')};
}

export async function cargo(args, {env = process.env, cwd = ROOT, skiaArchive} = {}) {
  const needed = requirements(args, env);
  const childEnv = needed ? (await prepareDependencies({...needed, env, skiaArchive})).env : {...env};
  const result = spawnSync('cargo', args, {cwd, env: childEnv, stdio: 'inherit'});
  if (result.error) throw result.error;
  if (result.signal) throw Error(`Cargo terminated by ${result.signal}`);
  return result.status ?? 1;
}

if (path.resolve(process.argv[1] || '') === fileURLToPath(import.meta.url)) {
  try {
    const args = process.argv.slice(2), i = args.indexOf('--skia-archive');
    let skiaArchive;
    if (i >= 0) {skiaArchive = args[i + 1]; if (!skiaArchive) throw Error('Missing --skia-archive value'); args.splice(i, 2);}
    process.exitCode = await cargo(args, {skiaArchive});
  } catch (error) {console.error(error.message); process.exitCode = 1;}
}
