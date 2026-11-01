// Run with: node --test scripts/extensions/package
import { deepStrictEqual, equal, ok, throws } from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';
import { test } from 'node:test';
import { gunzipSync } from 'node:zlib';
import { manifestProblems, pack, safePath } from './pack.mjs';
import { createTar } from './tar.mjs';

const manifest = {
  schema: 1, id: 'acme.night', version: '1.0.0', name: 'Acme Night', publisher: 'Acme',
  description: 'A quieter palette.', categories: ['Themes'], license: 'MIT',
  repository: 'https://example.com/acme', icon: 'icon.svg', readme: 'README.md', changelog: 'CHANGELOG.md',
  engines: { loom: '0.1.8' }, contributes: { themes: [{ id: 'acme.night.dark', path: 'themes/dark.json' }] },
};

function folder(overrides = {}, drop = []) {
  const dir = mkdtempSync(join(tmpdir(), 'loom-pack-test-'));
  const files = { 'manifest.json': JSON.stringify({ ...manifest, ...overrides }), 'icon.svg': '<svg/>', 'README.md': '# Acme', 'CHANGELOG.md': '## 1.0.0', 'themes/dark.json': '{}' };
  for (const [name, text] of Object.entries(files)) {
    if (drop.includes(name)) continue;
    mkdirSync(join(dir, name, '..'), { recursive: true });
    writeFileSync(join(dir, name), text);
  }
  return dir;
}

test('a complete folder is packed, the archive lists its files, and packing again gives the same bytes', () => {
  const dir = folder(), out = mkdtempSync(join(tmpdir(), 'loom-pack-out-'));
  try {
    const first = pack(dir, out);
    equal(first.files, 5);
    equal(first.id, 'acme.night');
    const bytes = readFileSync(first.file);
    const listing = spawnSync('tar', ['-tzf', first.file], { encoding: 'utf8' });
    if (listing.status === 0) deepStrictEqual(listing.stdout.split(/\r?\n/).filter(Boolean).sort(), ['CHANGELOG.md', 'README.md', 'icon.svg', 'manifest.json', 'themes/dark.json'].map((name) => `acme.night/${name}`));
    equal(pack(dir, out).sha256, first.sha256, 'deterministic');
    equal(readFileSync(`${first.file}.sha256`, 'utf8').split(' ')[0], first.sha256);
    equal(gunzipSync(bytes).length % 512, 0, 'a whole number of tar blocks');
  } finally { rmSync(dir, { recursive: true, force: true }); rmSync(out, { recursive: true, force: true }); }
});

test('everything missing from the manifest is listed at once', () => {
  const problems = manifestProblems({ schema: 1, id: 'a.b', version: '0.1.0' }, () => undefined);
  for (const field of ['name', 'publisher', 'description', 'categories', 'license', 'repository', 'icon', 'readme', 'changelog', 'engines.loom']) {
    ok(problems.some((p) => p.startsWith(`\`${field}\``)), `${field}: ${problems.join(' | ')}`);
  }
});

test('wrong values and missing files are refused with a reason', () => {
  const dir = folder({ categories: ['Nonsense'], repository: 'http://example.com', icon: 'icon.gif' }, []);
  const out = mkdtempSync(join(tmpdir(), 'loom-pack-out-'));
  try {
    throws(() => pack(dir, out), (error) => /is not a category/.test(error.message) && /https:\/\//.test(error.message) && /PNG or SVG/.test(error.message));
  } finally { rmSync(dir, { recursive: true, force: true }); rmSync(out, { recursive: true, force: true }); }
  const missing = folder({}, ['README.md', 'themes/dark.json']);
  try {
    throws(() => pack(missing, out), (error) => /README.md is not in the package/.test(error.message) && /themes\/dark.json is not in the package/.test(error.message));
  } finally { rmSync(missing, { recursive: true, force: true }); rmSync(out, { recursive: true, force: true }); }
});

test('paths that could leave the package are not paths', () => {
  for (const bad of ['../x', '/x', 'a/../b', 'a\\b', 'C:x', 'a/', '', 'a/.', 'trail.', 'a:b']) ok(!safePath(bad), bad);
  for (const good of ['a', 'a/b.json', 'themes/dark.json', '.hidden/file']) ok(safePath(good), good);
});

test('long paths use the prefix field and the checksum is valid', () => {
  const path = `${'d'.repeat(60)}/${'e'.repeat(60)}/file.txt`;
  const tar = createTar([{ path, data: Buffer.from('hello') }]);
  equal(tar.length, 512 * 4);
  let sum = 0;
  for (let i = 0; i < 512; i++) sum += i >= 148 && i < 156 ? 0x20 : tar[i];
  equal(parseInt(tar.subarray(148, 154).toString(), 8), sum);
  throws(() => createTar([{ path: 'x'.repeat(300), data: Buffer.alloc(0) }]), /too long/);
});
