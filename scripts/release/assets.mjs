// Validate all three platforms locally before creating or changing a draft.
import fs from 'node:fs';
import path from 'node:path';
import {sha256File} from '../package/lib/util.mjs';
import {validateVersion} from './notes.mjs';
export const platforms = {
  'windows-x86_64': {setup:'-setup.exe',portable:'-portable.zip'},
  'linux-x86_64': {deb:'.deb',appimage:'.AppImage',tarball:'.tar.gz'},
  'macos-universal': {dmg:'.dmg',zip:'.zip'},
};
// loom-ext-validate, which the extensions repository downloads to check its packages.
// The name carries no version: the release tag is part of the download address.
export const validators = ['windows-x86_64.exe','linux-x86_64','macos-aarch64'].map(suffix => 'loom-ext-validate-' + suffix);
export function verifyAssets(directory,version) {
  validateVersion(version);
  const files = [], metadata = new Map();
  const requireFile = name => {
    const file = path.join(directory,name);
    if (!fs.existsSync(file) || !fs.lstatSync(file).isFile()) throw Error('Missing release asset: ' + name);
    return file;
  };
  for (const [target,formats] of Object.entries(platforms)) {
    const prefix = 'Loom-' + version + '-' + target;
    for (const suffix of Object.values(formats)) {
      const name = prefix + suffix, file = requireFile(name);
      metadata.set(name,{sha256:sha256File(file),size:fs.statSync(file).size});files.push(file);
    }
    const checksums = requireFile(prefix + '-SHA256SUMS.txt');files.push(checksums);
    const lines = fs.readFileSync(checksums,'utf8').trim().split(/\r?\n/).map(line => /^([a-f0-9]{64}) [ *](.+)$/i.exec(line));
    const expected = Object.values(formats).map(suffix => prefix + suffix);
    if (lines.length !== expected.length || lines.some(match => !match || !expected.includes(match[2])) || new Set(lines.map(match => match?.[2])).size !== expected.length) throw Error('Invalid checksum inventory: ' + target);
    for (const match of lines) if (metadata.get(match[2]).sha256 !== match[1].toLowerCase()) throw Error('Checksum mismatch: ' + match[2]);
    const manifest = requireFile('update-' + target + '.json');files.push(manifest);
    const envelope = JSON.parse(fs.readFileSync(manifest,'utf8'));
    const payload = JSON.parse(Buffer.from(envelope.payload,'base64').toString('utf8'));
    if (payload.version !== version || payload.platform !== target || payload.schema !== 1 || Buffer.from(envelope.signature || '', 'base64').length !== 64) throw Error('Invalid update manifest: ' + target);
    if (Object.keys(payload.assets || {}).sort().join(',') !== Object.keys(formats).sort().join(',')) throw Error('Invalid update assets: ' + target);
    for (const [kind,suffix] of Object.entries(formats)) {
      const name = prefix + suffix, record = payload.assets[kind], actual = metadata.get(name);
      if (record.name !== name || record.sha256 !== actual.sha256 || record.size !== actual.size) throw Error('Update manifest mismatch: ' + name);
    }
  }
  for (const name of validators) {
    const file = requireFile(name);
    if (fs.statSync(file).size === 0) throw Error('Empty release asset: ' + name);
    files.push(file);
  }
  return files.map(file => path.resolve(file)).sort();
}
