// Signed update manifests.
//
// Each release carries one `update-<platform>.json` per platform. Loog reads
// it from `releases/latest/download/`, verifies the Ed25519 signature with the
// public key compiled into `src/infrastructure/update.rs` and offers the
// release when it is newer than the running build.
//
// The file is an envelope around the exact signed bytes:
//
//   { "payload": "<base64 JSON>", "signature": "<base64 Ed25519 signature>" }
//
// The payload names its platform, version, release page and the SHA-256 and
// size of every package, so a manifest cannot be replayed on another platform
// and a package cannot be swapped without breaking the signature.
//
// The private key comes from LOOG_UPDATE_SIGNING_KEY (PKCS#8 PEM text) or
// LOOG_UPDATE_SIGNING_KEY_FILE (a path to it). `keygen` creates a new pair.

import { createPrivateKey, createPublicKey, generateKeyPairSync, sign } from 'node:crypto';
import fs from 'node:fs';
import path from 'node:path';

import { DIST_DIR, ROOT, WORK_DIR, cargoPackageField, fail, info, sha256File, step } from './lib/util.mjs';

/** Packages per platform, keyed by the asset names Loog looks up. */
const ASSETS = {
  windows: { target: 'windows-x86_64', files: { setup: '-setup.exe', portable: '-portable.zip' } },
  linux: { target: 'linux-x86_64', files: { deb: '.deb', appimage: '.AppImage', tarball: '.tar.gz' } },
  macos: { target: 'macos-universal', files: { dmg: '.dmg', zip: '.zip' } },
};

function signingKey() {
  const pem = process.env.LOOG_UPDATE_SIGNING_KEY?.trim()
    || (process.env.LOOG_UPDATE_SIGNING_KEY_FILE
      && fs.readFileSync(process.env.LOOG_UPDATE_SIGNING_KEY_FILE, 'utf8'));
  if (!pem) {
    return undefined;
  }
  const key = createPrivateKey(pem);
  if (key.asymmetricKeyType !== 'ed25519') {
    fail('The update signing key must be an Ed25519 key.');
  }
  return key;
}

/** The raw 32-byte public key, base64, as `update.rs` embeds it. */
function rawPublicKey(key) {
  const der = createPublicKey(key).export({ format: 'der', type: 'spki' });
  return der.subarray(der.length - 32).toString('base64');
}

/**
 * Write `update-<target>.json` into the dist folder. Without a signing key the
 * step fails when `required`, and is skipped otherwise.
 */
export function writeManifest({ platform, version, required }) {
  const spec = ASSETS[platform];
  if (!spec) {
    fail(`Unknown platform: ${platform}`);
  }
  step('Writing the update manifest');
  const key = signingKey();
  if (!key) {
    if (required) {
      fail('Set LOOG_UPDATE_SIGNING_KEY or LOOG_UPDATE_SIGNING_KEY_FILE to sign the update manifest.');
    }
    info('Skipped: no update signing key is set.');
    return;
  }

  const repository = cargoPackageField('repository').replace(/\/+$/, '');
  const tag = `v${version}`;
  const prefix = `Loog-${version}-${spec.target}`;
  const assets = {};
  for (const [name, suffix] of Object.entries(spec.files)) {
    const file = `${prefix}${suffix}`;
    const full = path.join(DIST_DIR, file);
    if (!fs.existsSync(full)) {
      fail(`${path.relative(ROOT, full)} is missing. Run the \`bundle\` step first.`);
    }
    assets[name] = {
      name: file,
      url: `${repository}/releases/download/${tag}/${file}`,
      sha256: sha256File(full),
      size: fs.statSync(full).size,
    };
  }
  const payload = Buffer.from(JSON.stringify({
    schema: 1,
    platform: spec.target,
    version,
    notes_url: `${repository}/releases/tag/${tag}`,
    assets,
  }));
  const envelope = {
    payload: payload.toString('base64'),
    signature: sign(null, payload, key).toString('base64'),
  };
  const output = path.join(DIST_DIR, `update-${spec.target}.json`);
  fs.writeFileSync(output, `${JSON.stringify(envelope, null, 2)}\n`);
  info(`${path.relative(ROOT, output)} (public key ${rawPublicKey(key)})`);
}

/** Create a key pair: the private key goes to a file, the public key to stdout. */
export function keygen() {
  const file = path.join(WORK_DIR, 'update-signing-key.pem');
  if (fs.existsSync(file)) {
    fail(`${path.relative(ROOT, file)} already exists; move it away to create a new key.`);
  }
  const { privateKey } = generateKeyPairSync('ed25519');
  fs.mkdirSync(WORK_DIR, { recursive: true });
  fs.writeFileSync(file, privateKey.export({ format: 'pem', type: 'pkcs8' }), { mode: 0o600 });
  step('Created an update signing key');
  info(`Private key: ${path.relative(ROOT, file)}`);
  info('Store it as the LOOG_UPDATE_SIGNING_KEY repository secret, keep a backup, then delete the file.');
  info(`Public key for src/infrastructure/update.rs: ${rawPublicKey(privateKey)}`);
}
