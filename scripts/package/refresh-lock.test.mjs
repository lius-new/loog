import assert from 'node:assert/strict';
import crypto from 'node:crypto';
import test from 'node:test';
import {buildLock, trustedKeys, verifySignature} from './lib/refresh.mjs';

const SPKI_PREFIX = Buffer.from('302a300506032b6570032100', 'hex');

function signed(bytes, keyId = 'test-1') {
  const {publicKey, privateKey} = crypto.generateKeyPairSync('ed25519');
  const raw = publicKey.export({type: 'spki', format: 'der'}).subarray(SPKI_PREFIX.length).toString('base64');
  const signature = crypto.sign(null, bytes, privateKey).toString('base64');
  return {keys: {[keyId]: raw}, file: JSON.stringify({keyId, algorithm: 'ed25519', signature})};
}

test('the trusted keys are read from the application source', () => {
  const keys = trustedKeys();
  assert.ok(Object.keys(keys).length >= 1);
  for (const raw of Object.values(keys)) assert.equal(Buffer.from(raw, 'base64').length, 32);
});

test('a signature is accepted only for the index it signed, by a trusted key', () => {
  const bytes = Buffer.from('{"sequence":1}\n');
  const {keys, file} = signed(bytes);
  assert.equal(verifySignature(bytes, file, keys), 'test-1');
  assert.throws(() => verifySignature(Buffer.from('{"sequence":2}\n'), file, keys), /does not match/);
  assert.throws(() => verifySignature(bytes, file, {}), /unknown key/);
  assert.throws(() => verifySignature(bytes, 'not json', keys), /not JSON/);
});

test('the lock names the newest version of each bundled extension and the index it came from', () => {
  const artifact = {platform: 'any', url: 'https://example.test/a.loogext', sha256: 'a'.repeat(64), size: 1};
  const index = {sequence: 17, keyId: 'test-1', extensions: [
    {id: 'loog.theme.aura', versions: [{version: '0.1.0', artifacts: [artifact]}]},
    {id: 'loog.lang.json', versions: [{version: '0.1.0', artifacts: [artifact]}]},
  ]};
  const bytes = Buffer.from(JSON.stringify(index));
  const lock = buildLock(index, bytes, ['loog.theme.aura']);
  assert.deepEqual(lock.extensions, [{id: 'loog.theme.aura', version: '0.1.0', artifacts: [artifact]}]);
  assert.equal(lock.index.sequence, 17);
  assert.equal(lock.index.sha256, crypto.createHash('sha256').update(bytes).digest('hex'));
  assert.throws(() => buildLock(index, bytes, ['loog.lang.missing']), /not in the index/);
});
