import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {execFileSync, spawnSync} from 'node:child_process';
import {archive, extract, sha256, tar} from './cache.mjs';
import {cmakeSpec} from './cmake.mjs';
import {skiaSpec, prepareSkia} from './skia.mjs';
import {MANIFEST, ROOT, scopedEnvironment, prepareDependencies} from './ensure.mjs';
import {requirements} from '../build/cargo.mjs';

function fixture(t) {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'loog-dependencies-'));
  t.after(() => fs.rmSync(directory, {recursive: true, force: true}));
  return directory;
}
const digest = bytes => createHash('sha256').update(bytes).digest('hex');
const input = bytes => ({file: 'fixture.tar.gz', url: 'https://example.invalid/fixture', sha256: digest(bytes)});
const offlineFetch = () => {throw Error('Network must not be used');};

test('cold download is pinned, offline reuse verifies bytes, corruption fails offline and repairs online', async t => {
  const cache = fixture(t), bytes = Buffer.from('pinned archive'), spec = input(bytes);
  let downloads = 0;
  const fetcher = async () => {downloads++; return new Response(bytes);};
  const file = await archive(spec, {cache, fetcher});
  assert.equal(await sha256(file), spec.sha256);
  assert.equal(await archive(spec, {cache, offline: true, fetcher: offlineFetch}), file);
  fs.writeFileSync(file, 'corrupt');
  await assert.rejects(archive(spec, {cache, offline: true, fetcher: offlineFetch}), /corrupt dependency cache/);
  assert.equal(await archive(spec, {cache, fetcher}), file);
  assert.equal(downloads, 2);
});

test('missing cache offline, failed HTTP, wrong digest and interrupted streams never publish a cache file', async t => {
  const cache = fixture(t), spec = input('expected');
  await assert.rejects(archive(spec, {cache, offline: true}), /online first/);
  await assert.rejects(archive(spec, {cache, wait: async () => {}, fetcher: async () => new Response('no', {status: 503})}), /503/);
  await assert.rejects(archive(spec, {cache, fetcher: async () => new Response('wrong')}), /SHA-256/);
  const interrupted = async () => new Response(new ReadableStream({start(controller) {controller.enqueue(new Uint8Array([1])); controller.error(Error('interrupted'));}}));
  await assert.rejects(archive(spec, {cache, fetcher: interrupted}), /interrupted/);
  assert.deepEqual(fs.readdirSync(path.join(cache, 'archives', spec.sha256)), []);
});

test('concurrent downloaders only publish verified complete archives', async t => {
  const cache = fixture(t), spec = input('same');
  const files = await Promise.all(Array.from({length: 4}, () => archive(spec, {cache, fetcher: async () => new Response('same')})));
  assert.equal(new Set(files).size, 1);
  assert.equal(await sha256(files[0]), spec.sha256);
  assert.deepEqual(fs.readdirSync(path.dirname(files[0])), [spec.file]);
});

test('extraction reuses complete files, repairs altered resources, and serializes concurrent preparers', async t => {
  const cache = fixture(t), spec = input('archive');
  let extractions = 0;
  const extractor = args => {
    if (args[0] === '-tf') return 'tool/bin/cmake\ntool/share/module.cmake\n';
    extractions++;
    const target = args.at(-1);
    fs.mkdirSync(path.join(target, 'tool/bin'), {recursive: true});
    fs.mkdirSync(path.join(target, 'tool/share'), {recursive: true});
    fs.writeFileSync(path.join(target, 'tool/bin/cmake'), 'executable');
    fs.writeFileSync(path.join(target, 'tool/share/module.cmake'), 'module');
    return '';
  };
  const prepared = await Promise.all(Array.from({length: 3}, () => extract(spec, 'archive', {cache, extractor})));
  assert.equal(new Set(prepared).size, 1); assert.equal(extractions, 1);
  fs.writeFileSync(path.join(prepared[0], 'tool/share/module.cmake'), 'damaged');
  await extract(spec, 'archive', {cache, extractor}); assert.equal(extractions, 2);
  assert.equal(fs.readFileSync(path.join(prepared[0], 'tool/share/module.cmake'), 'utf8'), 'module');
});

test('unsafe paths and extraction failure leave no partial installed tools', async t => {
  const cache = fixture(t), spec = input('archive');
  for (const name of ['../escape', '/absolute', 'C:/escape', 'tool/../../escape', 'tool\\..\\escape']) {
    await assert.rejects(extract(spec, 'file', {cache, extractor: () => name}), /Unsafe/);
  }
  await assert.rejects(extract(spec, 'file', {cache, extractor: args => {if (args[0] === '-tf') return 'valid'; throw Error('extract failed');}}), /extract failed/);
  assert.deepEqual(fs.readdirSync(path.join(cache, 'tools', spec.sha256)), []);
});

test('CMake selection follows host, Skia follows target, including both universal macOS slices', () => {
  for (const [platform, arch] of [['win32','x64'],['win32','arm64'],['linux','x64'],['linux','arm64'],['darwin','x64'],['darwin','arm64']]) {
    assert.match(cmakeSpec(MANIFEST, platform, arch).sha256, /^[0-9a-f]{64}$/);
  }
  assert.throws(() => cmakeSpec(MANIFEST, 'freebsd', 'x64'), /Unsupported/);
  assert.equal(cmakeSpec(MANIFEST, 'darwin', 'arm64').sha256, cmakeSpec(MANIFEST, 'darwin', 'x64').sha256);
  const lock = fs.readFileSync(path.join(ROOT, 'Cargo.lock'), 'utf8');
  for (const [target, spec] of Object.entries(MANIFEST.skia.targets)) {
    assert.equal(skiaSpec(MANIFEST, target, lock, `skia-bindings v0.99.0|${spec.features}\n`), spec);
    assert.ok(spec.key.includes(target)); assert.match(spec.sha256, /^[0-9a-f]{64}$/);
    assert.throws(() => skiaSpec(MANIFEST, target, lock, 'changed'), /feature set/);
    assert.throws(() => skiaSpec(MANIFEST, target, lock.replace('name = "skia-bindings"\nversion = "0.99.0"', 'name = "skia-bindings"\nversion = "1.0.0"').replace('name = "skia-bindings"\r\nversion = "0.99.0"', 'name = "skia-bindings"\r\nversion = "1.0.0"'), ''), /Cargo.lock/);
  }
  assert.throws(() => skiaSpec(MANIFEST, 'aarch64-pc-windows-msvc', lock, ''), /Unsupported/);
});

test('build environment takes managed CMake first, preserves parent env, and replaces host Skia for a cross target', () => {
  for (const [platform, delimiter] of [['win32',';'],['linux',':'],['darwin',':']]) {
    const env = {Path: '/system', SKIA_BINARIES_URL: 'wrong-host', FORCE_SKIA_BUILD: '1', FORCE_SKIA_BINARIES_DOWNLOAD:'1', SKIA_SOURCE_DIR:'/source', SKIA_LIBRARY_SEARCH_PATH:'/system-skia', KEEP: 'value'};
    const result = scopedEnvironment(env, {bin:'/private/cmake', executable:'/private/cmake/cmake'}, {url:'file:///private/target-skia'}, platform);
    assert.equal(result.Path, '/private/cmake' + delimiter + '/system');
    assert.equal(result.CMAKE, '/private/cmake/cmake');
    assert.equal(result.SKIA_BINARIES_URL, 'file:///private/target-skia');
    assert.equal(result.FORCE_SKIA_BINARIES_DOWNLOAD, undefined); assert.equal(result.FORCE_SKIA_BUILD, undefined);
    assert.equal(result.SKIA_SOURCE_DIR, undefined); assert.equal(result.SKIA_LIBRARY_SEARCH_PATH, undefined);
    assert.equal(result.KEEP, 'value'); assert.equal(env.Path, '/system'); assert.equal(env.SKIA_BINARIES_URL, 'wrong-host');
  }
});

test('Skia explicit archive accepts spaces and checks digest and embedded version/key before Cargo', async t => {
  const root = fixture(t), source = path.join(root, 'source'), cache = path.join(root, 'cache');
  fs.mkdirSync(path.join(source, 'skia-binaries'), {recursive: true});
  fs.writeFileSync(path.join(source, 'skia-binaries/key.txt'), 'test-key');
  fs.writeFileSync(path.join(source, 'skia-binaries/tag.txt'), '0.99.0');
  const file = path.join(root, 'Skia archive.tar.gz');
  tar(['-czf', file, '-C', source, 'skia-binaries']);
  const spec = {...input(''), key:'test-key', sha256: await sha256(file)};
  const options = {cache, manifest:MANIFEST, skiaArchive:file, offline:true};
  assert.equal((await prepareSkia(spec, options)).url, 'file://' + file.replaceAll('\\','/'));
  await assert.rejects(prepareSkia({...spec, key:'wrong'}, options), /key mismatch/);
  await assert.rejects(prepareSkia(spec, {...options, manifest:{skia:{version:'wrong'}}}), /version\/key/);
  fs.writeFileSync(file, 'corrupt');
  await assert.rejects(prepareSkia(spec, options), /SHA-256/);
  await assert.rejects(prepareSkia(spec, {...options, skiaArchive:file+'missing'}), /missing/);
});

test('Cargo wrapper skips dependency downloads for core-only and utility commands, recognizes target/offline', () => {
  for (const args of [['fmt','--all'],['--version'],['tree'],['test','--locked','-p','loog-editor-core'],['test','--package=loog-workspace-tests']]) assert.equal(requirements(args), null);
  assert.deepEqual(requirements(['test','--workspace','-p','loog-editor-core'], {}), {target:undefined, offline:false});
  assert.deepEqual(requirements(['build','--target=aarch64-apple-darwin','--offline']), {target:'aarch64-apple-darwin', offline:true});
  assert.deepEqual(requirements(['build','--target','x86_64-apple-darwin','--frozen']), {target:'x86_64-apple-darwin', offline:true});
  assert.equal(requirements(['build'], {CARGO_BUILD_TARGET:'x86_64-apple-darwin'}).target, 'x86_64-apple-darwin');
  const wrapper = path.join(ROOT, 'scripts/build/cargo.mjs');
  assert.match(execFileSync(process.execPath, [wrapper, '--version'], {encoding:'utf8'}), /cargo/);
  const failed = spawnSync(process.execPath, [wrapper, 'help', '--invalid-loog-option'], {encoding:'utf8'});
  assert.notEqual(failed.status, 0);
});

test('unsupported features and missing offline cache fail before any Cargo compilation', async t => {
  await assert.rejects(prepareDependencies({env:{...process.env,SKIA_DEBUG:'1'}, cache:fixture(t)}), /Pinned Skia/);
  await assert.rejects(prepareDependencies({skia:false, offline:true, cache:fixture(t)}), /online first/);
});

test('native CI prepares once; release compile owns target preparation and setup never runs tests', () => {
  const read = file => fs.readFileSync(path.join(ROOT,file),'utf8');
  const ci = read('.github/workflows/ci.yml');
  const core = ci.slice(ci.search(/^  core:/m),ci.search(/^  build:/m));
  assert.ok(!core.includes('actions/dependencies'));
  const host = ci.slice(ci.search(/^  build:/m),ci.search(/^  result:/m));
  assert.ok(host.indexOf('uses: ./.github/actions/dependencies') < host.indexOf('cargo test --locked -p loog'));
  const action = read('.github/actions/dependencies/action.yml');
  assert.ok(action.includes('node scripts/dependencies/ensure.mjs --github'));
  assert.doesNotMatch(action,/node --test|smoke\.mjs/);
  assert.ok(action.includes('runner.os') && action.includes('runner.arch') && action.includes('manifest.json'));
  for (const os of ['windows','linux','macos']) {
    const code = read(`scripts/package/${os}/index.mjs`);
    assert.ok(code.includes('await prepareDependencies('));
    if (os === 'macos') assert.ok(code.includes('prepareDependencies({target, env})'));
  }
  for (const file of ['scripts/architecture/standalone-host.mjs']) assert.ok(read(file).includes('await prepareDependencies('));
  assert.ok(read('scripts/baseline/run.mjs').includes('../dependencies/ensure.mjs'));
});

test('cold dependency download recovers from a temporary upstream failure', async t => {
  const cache = fixture(t), bytes = Buffer.from('recovered archive'), spec = input(bytes); let calls = 0;
  const file = await archive(spec, {cache, wait:async () => {}, fetcher:async () =>
    ++calls === 1 ? new Response('', {status:503}) : new Response(bytes)});
  assert.equal(calls,2); assert.equal(await sha256(file), spec.sha256);
  assert.deepEqual(fs.readdirSync(path.dirname(file)), [spec.file]);
});
