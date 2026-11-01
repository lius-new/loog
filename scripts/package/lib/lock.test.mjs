import test from 'node:test';
import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {gzipSync} from 'node:zlib';
import {createTar} from '../../extensions/package/tar.mjs';
import {fetchLocked, readLock, stageBundled} from './lock.mjs';

const sha = bytes => crypto.createHash('sha256').update(bytes).digest('hex');

/** A package of a theme extension, with the root folder named after its id. */
function archive(id, root = id) {
  const manifest = {schema: 1, id, version: '1.0.0', contributes: {themes: [{id: `${id}.a`, path: 'a.json'}]}};
  return gzipSync(createTar([
    {path: `${root}/a.json`, data: Buffer.from('{}')},
    {path: `${root}/manifest.json`, data: Buffer.from(JSON.stringify(manifest))},
  ]));
}

function fixture(t, packages) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'loom-lock-'));
  t.after(() => fs.rmSync(root, {recursive: true, force: true}));
  const served = new Map();
  const lock = {
    schema: 1, index: {sequence: 1, keyId: 'k', sha256: '0'.repeat(64)},
    extensions: packages.map(({id, bytes, platform = 'any', size}) => {
      const url = `https://example.com/${id}.loomext`;
      served.set(url, bytes);
      return {id, version: '1.0.0', artifacts: [{platform, url, sha256: sha(bytes), size: size ?? bytes.length}]};
    }),
  };
  const fetchBytes = async url => served.get(url);
  return {root, lock, fetchBytes};
}

test('locked packages are downloaded, checked against the lock and staged like bundled ones', async t => {
  const {root, lock, fetchBytes} = fixture(t, [{id: 'loom.look', bytes: archive('loom.look')}]);
  const locked = path.join(root, 'locked');
  assert.deepEqual(await fetchLocked({lock, destination: locked, fetchBytes}), ['loom.look']);

  const registry = path.join(root, 'registry');
  fs.mkdirSync(registry);
  fs.writeFileSync(path.join(registry, 'extensions.lock'), JSON.stringify(lock));
  fs.writeFileSync(path.join(registry, 'index.snapshot.json'), '{}');
  fs.writeFileSync(path.join(registry, 'index.snapshot.json.sig'), '{}');
  const destination = path.join(root, 'app/extensions');
  stageBundled({destination, lock: path.join(registry, 'extensions.lock'), locked, registry});
  assert.equal(fs.readFileSync(path.join(destination, 'loom.look/a.json'), 'utf8'), '{}');
  // The lock and the snapshot go beside the extensions folder.
  assert.equal(fs.readFileSync(path.join(root, 'app/registry/index.snapshot.json'), 'utf8'), '{}');
});

test('a package that is not what the lock says stops the release', async t => {
  const good = archive('loom.look');
  for (const [problem, entry, expected] of [
    ['other bytes', {id: 'loom.look', bytes: good, size: good.length + 1}, /does not match|expected/],
    ['another root folder', {id: 'loom.look', bytes: archive('loom.look', 'loom.other')}, /unexpected entry/],
    ['a platform build', {id: 'loom.look', bytes: good, platform: 'linux-x86_64'}, /every platform/],
  ]) {
    const {root, lock, fetchBytes} = fixture(t, [entry]);
    await assert.rejects(fetchLocked({lock, destination: path.join(root, 'locked'), fetchBytes}), expected, problem);
  }
  const damaged = fixture(t, [{id: 'loom.look', bytes: good}]);
  damaged.lock.extensions[0].artifacts[0].sha256 = '1'.repeat(64);
  await assert.rejects(fetchLocked({lock: damaged.lock, destination: path.join(damaged.root, 'locked'), fetchBytes: damaged.fetchBytes}), /does not match the lock/);
});

test('without a lock the bundled folders are used, and a lock without its fetch is an error', t => {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'loom-lock-none-'));
  t.after(() => fs.rmSync(root, {recursive: true, force: true}));
  assert.equal(readLock(path.join(root, 'missing.lock')), null);
  const file = path.join(root, 'extensions.lock');
  fs.writeFileSync(file, JSON.stringify({schema: 1, extensions: [{id: 'loom.look', version: '1.0.0', artifacts: []}]}));
  assert.throws(() => stageBundled({destination: path.join(root, 'out/extensions'), lock: file, locked: path.join(root, 'none'), registry: root}), /not fetched/);
  fs.writeFileSync(file, '{"schema":2}');
  assert.throws(() => readLock(file), /not a usable lock/);
});
