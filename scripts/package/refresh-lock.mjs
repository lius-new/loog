// Refresh assets/registry from the extensions repository's live, signed index.
//
//   node scripts/package/refresh-lock.mjs [--bundle id,id,...]
//
// Downloads the registry's index and signature, checks the signature against the
// public keys the application itself trusts (src/infrastructure/extensions/registry.rs),
// and rewrites extensions.lock, index.snapshot.json and index.snapshot.json.sig for the
// newest version of each bundled extension. The bundled set defaults to the one in the
// current lock. Commit the result, wait for CI, then tag the release: the lock stays
// part of the tested commit, so extension releases never have to notify Loom.
import crypto from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {LOCK_FILE, REGISTRY_DIR, readLock} from './lib/lock.mjs';

const ROOT = fileURLToPath(new URL('../../', import.meta.url));
const REGISTRY_URL = 'https://github.com/lius-new/loom-extensions/releases/download/registry';
const KEYS_SOURCE = path.join(ROOT, 'src/infrastructure/extensions/registry.rs');
const SPKI_PREFIX = Buffer.from('302a300506032b6570032100', 'hex');

/** The trusted keys, read from the one place the application defines them. */
export function trustedKeys(source = fs.readFileSync(KEYS_SOURCE, 'utf8')) {
  const block = /pub const KEYS: &\[\(&str, &str\)\] = &\[([\s\S]*?)\];/.exec(source);
  if (!block) throw new Error(`${KEYS_SOURCE}: cannot find the trusted index keys`);
  const keys = {};
  for (const [, id, raw] of block[1].matchAll(/\(\s*"([\w.-]+)",\s*"([A-Za-z0-9+/=]+)",?\s*\)/g)) keys[id] = raw;
  if (!Object.keys(keys).length) throw new Error(`${KEYS_SOURCE}: no trusted index keys`);
  return keys;
}

/** Whether `signature` (the .sig file text) is a good signature of `bytes` by a trusted key. */
export function verifySignature(bytes, signature, keys) {
  let envelope;
  try { envelope = JSON.parse(signature); } catch { throw new Error('The index signature is not JSON'); }
  if (envelope.algorithm !== 'ed25519') throw new Error('The index signature uses an unsupported algorithm');
  const raw = keys[envelope.keyId];
  if (!raw) throw new Error(`The index is signed with an unknown key ${envelope.keyId}`);
  const key = crypto.createPublicKey({key: Buffer.concat([SPKI_PREFIX, Buffer.from(raw, 'base64')]), format: 'der', type: 'spki'});
  if (!crypto.verify(null, bytes, key, Buffer.from(envelope.signature, 'base64'))) throw new Error('The index signature does not match the index');
  return envelope.keyId;
}

/** The lock for the newest version of each bundled extension in a verified index. */
export function buildLock(index, bytes, ids) {
  const extensions = ids.map(id => {
    const found = index.extensions?.find(extension => extension.id === id);
    if (!found) throw new Error(`${id} is bundled but not in the index`);
    const {version, artifacts} = found.versions[0];
    return {id, version, artifacts};
  });
  return {
    schema: 1,
    index: {sequence: index.sequence, keyId: index.keyId, sha256: crypto.createHash('sha256').update(bytes).digest('hex')},
    extensions,
  };
}

async function download(name) {
  const response = await fetch(`${REGISTRY_URL}/${name}`);
  if (!response.ok) throw new Error(`${REGISTRY_URL}/${name}: HTTP ${response.status}`);
  return Buffer.from(await response.arrayBuffer());
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const args = process.argv.slice(2);
    if (args.length && !(args.length === 2 && args[0] === '--bundle')) throw new Error('Usage: node scripts/package/refresh-lock.mjs [--bundle id,id,...]');
    const ids = args[1]?.split(',') ?? readLock()?.extensions.map(extension => extension.id);
    if (!ids?.length) throw new Error('Name the bundled extensions with --bundle id,id,...');
    const bytes = await download('index.json');
    const signature = (await download('index.json.sig')).toString('utf8');
    const keyId = verifySignature(bytes, signature, trustedKeys());
    const index = JSON.parse(bytes);
    if (index.keyId !== keyId) throw new Error('The index keyId does not match its signature');
    const lock = buildLock(index, bytes, ids);
    fs.writeFileSync(LOCK_FILE, `${JSON.stringify(lock, null, 2)}\n`);
    fs.writeFileSync(path.join(REGISTRY_DIR, 'index.snapshot.json'), bytes);
    fs.writeFileSync(path.join(REGISTRY_DIR, 'index.snapshot.json.sig'), signature);
    console.log(`Locked ${lock.extensions.map(e => `${e.id} ${e.version}`).join(', ')} against index ${index.sequence}`);
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
