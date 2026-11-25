import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {execFileSync} from 'node:child_process';
import {install as installCommitChecks} from '../commit/install.mjs';
import {prepareCmake} from './cmake.mjs';
import {prepareSkia, skiaSpec} from './skia.mjs';

export const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
export const MANIFEST = JSON.parse(fs.readFileSync(new URL('./manifest.json', import.meta.url), 'utf8'));
export const hostTarget = (env = process.env) => execFileSync('rustc', ['-vV'], {env, encoding: 'utf8'}).match(/^host: (.+)$/m)?.[1];

export function prepareDeveloperCheckout(root = ROOT, env = process.env) {
  if (env.CI || env.GITHUB_ACTIONS) return;
  try {
    // Source archives have no .git; linked worktrees share another checkout's config.
    if (!fs.statSync(path.join(root, '.git'), {throwIfNoEntry: false})?.isDirectory()) return;
    installCommitChecks(root, env);
  } catch (error) {
    console.warn('Could not enable local Release Notes checks: ' + error.message);
  }
}

export function scopedEnvironment(env, cmake, skia, platform = process.platform) {
  const result = {...env};
  const key = Object.keys(result).find(key => key.toLowerCase() === 'path') || 'PATH';
  result[key] = cmake.bin + (platform === 'win32' ? ';' : ':') + (result[key] || '');
  result.CMAKE = cmake.executable;
  if (skia) {
    result.SKIA_BINARIES_URL = skia.url;
    // Upstream's FORCE flag is for source checkouts and requires .git;
    // registry builds must use their normal prebuilt path.
    delete result.FORCE_SKIA_BINARIES_DOWNLOAD;
    delete result.FORCE_SKIA_BUILD;
    delete result.SKIA_SOURCE_DIR;
    delete result.SKIA_LIBRARY_SEARCH_PATH;
  }
  return result;
}

export async function prepareDependencies({env = process.env, platform = process.platform, arch = process.arch,
  target, skia = true, offline = false, cache = path.join(ROOT, '.packaging/dependencies'), skiaArchive,
  root = ROOT, manifest = MANIFEST} = {}) {
  target ||= env.CARGO_BUILD_TARGET || hostTarget(env);
  const options = {env, platform, arch, target, offline, cache: path.resolve(cache), manifest,
    skiaArchive: skiaArchive ? path.resolve(skiaArchive) : undefined};
  let spec;
  if (skia) {
    if (env.SKIA_DEBUG || /target-feature=\+crt-static/.test([env.RUSTFLAGS, env.CARGO_ENCODED_RUSTFLAGS].join(' '))) throw Error('Pinned Skia archives support release Skia with dynamic CRT only.');
    const graph = execFileSync('cargo', ['tree', '--locked', ...(offline ? ['--offline'] : []), '--target', target,
      '-p', 'skia-bindings', '--depth', '0', '--prefix', 'none', '--format', '{p}|{f}'], {cwd: root, env, encoding: 'utf8'});
    spec = skiaSpec(manifest, target, fs.readFileSync(path.join(root, 'Cargo.lock'), 'utf8'), graph);
  }
  const cmake = await prepareCmake(manifest, options);
  const preparedSkia = skia ? await prepareSkia(spec, options) : undefined;
  return {env: scopedEnvironment(env, cmake, preparedSkia, platform), cmake, skia: preparedSkia, target};
}

if (path.resolve(process.argv[1] || '') === fileURLToPath(import.meta.url)) {
  try {
    const args = process.argv.slice(2);
    const value = name => {const i = args.indexOf(name); if (i < 0) return undefined; if (!args[i + 1] || args[i + 1].startsWith('--')) throw Error(`Missing value: ${name}`); return args[i + 1];};
    const allowed = new Set(['--offline', '--no-skia', '--github', '--json', '--target', '--cache', '--skia-archive']);
    for (let i = 0; i < args.length; i++) { if (!allowed.has(args[i])) throw Error(`Unknown dependency option: ${args[i]}`); if (['--target', '--cache', '--skia-archive'].includes(args[i])) i++; }
    const options = {target: value('--target'), cache: value('--cache'), skiaArchive: value('--skia-archive'), skia: !args.includes('--no-skia'), offline: args.includes('--offline')};
    if (!args.includes('--github')) prepareDeveloperCheckout();
    const result = await prepareDependencies(options);
    if (args.includes('--github')) {
      if (!process.env.GITHUB_PATH || !process.env.GITHUB_ENV) throw Error('--github requires GitHub Actions environment files');
      fs.appendFileSync(process.env.GITHUB_PATH, result.cmake.bin + '\n');
      fs.appendFileSync(process.env.GITHUB_ENV, `CMAKE=${result.cmake.executable}\n`);
      if (result.skia) fs.appendFileSync(process.env.GITHUB_ENV, `SKIA_BINARIES_URL=${result.skia.url}\n`);
    }
    const pathKey = Object.keys(result.env).find(key => key.toLowerCase() === 'path');
    console.log(JSON.stringify(args.includes('--json') ? {...result, env: {PATH: result.env[pathKey], CMAKE: result.env.CMAKE, SKIA_BINARIES_URL: result.env.SKIA_BINARIES_URL}} : {cmake: result.cmake.executable, skia: result.skia?.archive, target: result.target}));
  } catch (error) {console.error(error.message); process.exitCode = 1;}
}
