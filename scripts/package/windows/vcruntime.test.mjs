import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { crtFolderIn, isRuntimeDll, peImports, requiredRuntimeDlls, stageRuntime } from './vcruntime.mjs';

/** A minimal PE32+ image with one section that imports `imports` and delay-loads `delayed`. */
function image(imports, delayed = []) {
  const sectionRva = 0x1000;
  const sectionRaw = 0x200;
  const buffer = Buffer.alloc(0x200 + 0x400);
  buffer.writeUInt32LE(0x80, 0x3c);
  buffer.write('PE\0\0', 0x80, 'latin1');
  buffer.writeUInt16LE(1, 0x80 + 6); // one section
  buffer.writeUInt16LE(0xf0, 0x80 + 20); // optional header size
  const optional = 0x80 + 24;
  buffer.writeUInt16LE(0x20b, optional);
  const directories = optional + 112;

  // Section content: import descriptors, delay descriptors, then the names.
  let cursor = 0;
  const importTable = cursor;
  cursor += (imports.length + 1) * 20;
  const delayTable = cursor;
  cursor += (delayed.length + 1) * 32;
  const place = (name) => {
    const at = cursor;
    buffer.write(`${name}\0`, sectionRaw + at, 'latin1');
    cursor += name.length + 1;
    return sectionRva + at;
  };
  imports.forEach((name, index) => {
    buffer.writeUInt32LE(place(name), sectionRaw + importTable + index * 20 + 12);
  });
  delayed.forEach((name, index) => {
    buffer.writeUInt32LE(place(name), sectionRaw + delayTable + index * 32 + 4);
  });
  buffer.writeUInt32LE(sectionRva + importTable, directories + 1 * 8);
  buffer.writeUInt32LE(sectionRva + delayTable, directories + 13 * 8);

  const section = optional + 0xf0;
  buffer.writeUInt32LE(cursor, section + 8); // virtual size
  buffer.writeUInt32LE(sectionRva, section + 12);
  buffer.writeUInt32LE(0x400, section + 16); // raw size
  buffer.writeUInt32LE(sectionRaw, section + 20);
  return buffer;
}

function folder(t) {
  const dir = fs.mkdtempSync(path.join(os.tmpdir(), 'loog-vcruntime-'));
  t.after(() => fs.rmSync(dir, { recursive: true, force: true }));
  return dir;
}

test('runtime DLL names are recognised and system libraries are not', () => {
  for (const name of ['MSVCP140.dll', 'VCRUNTIME140.dll', 'vcruntime140_1.dll', 'msvcp140_atomic_wait.dll', 'concrt140.dll']) {
    assert.ok(isRuntimeDll(name), name);
  }
  for (const name of ['KERNEL32.dll', 'api-ms-win-crt-math-l1-1-0.dll', 'ucrtbase.dll', 'msvcp140d.dll.bak']) {
    assert.ok(!isRuntimeDll(name), name);
  }
});

test('import and delay-load tables are both read', () => {
  assert.deepEqual(peImports(image(['KERNEL32.dll', 'MSVCP140.dll'], ['ws2_32.dll'])), [
    'KERNEL32.dll',
    'MSVCP140.dll',
    'ws2_32.dll',
  ]);
  assert.deepEqual(peImports(image([])), []);
  assert.throws(() => peImports(Buffer.alloc(0x200)), /not a PE image/);
});

test('runtime DLLs are found through other runtime DLLs', (t) => {
  const dir = folder(t);
  const redist = path.join(dir, 'redist');
  fs.mkdirSync(redist);
  fs.writeFileSync(path.join(redist, 'msvcp140.dll'), image(['VCRUNTIME140.dll', 'VCRUNTIME140_1.dll']));
  fs.writeFileSync(path.join(redist, 'vcruntime140.dll'), image(['KERNEL32.dll']));
  fs.writeFileSync(path.join(redist, 'vcruntime140_1.dll'), image(['VCRUNTIME140.dll']));
  fs.writeFileSync(path.join(redist, 'concrt140.dll'), image([]));
  const exe = path.join(dir, 'Loog.exe');
  fs.writeFileSync(exe, image(['KERNEL32.dll', 'MSVCP140.dll']));

  assert.deepEqual(requiredRuntimeDlls(exe, redist), ['msvcp140.dll', 'vcruntime140.dll', 'vcruntime140_1.dll']);

  const stage = path.join(dir, 'stage');
  fs.mkdirSync(stage);
  assert.deepEqual(stageRuntime({ executable: exe, destination: stage, redistDir: redist }), [
    'msvcp140.dll',
    'vcruntime140.dll',
    'vcruntime140_1.dll',
  ]);
  assert.deepEqual(fs.readdirSync(stage).sort(), ['msvcp140.dll', 'vcruntime140.dll', 'vcruntime140_1.dll']);
});

test('a runtime DLL missing from the redistributable stops packaging', (t) => {
  const dir = folder(t);
  const exe = path.join(dir, 'Loog.exe');
  fs.writeFileSync(exe, image(['MSVCP140.dll']));
  assert.throws(() => requiredRuntimeDlls(exe, dir), /MSVCP140\.dll is required but missing/);
});

test('an executable without runtime imports stages nothing', (t) => {
  const dir = folder(t);
  const exe = path.join(dir, 'Loog.exe');
  fs.writeFileSync(exe, image(['KERNEL32.dll']));
  assert.deepEqual(requiredRuntimeDlls(exe, dir), []);
});

test('the newest redistributable version of an installation is chosen', (t) => {
  const installation = folder(t);
  for (const version of ['14.9.1', '14.44.35112', '14.10.2']) {
    fs.mkdirSync(path.join(installation, 'VC', 'Redist', 'MSVC', version, 'x64', 'Microsoft.VC143.CRT'), { recursive: true });
  }
  // A newer folder without an x64 CRT (for example only debug files) is skipped.
  fs.mkdirSync(path.join(installation, 'VC', 'Redist', 'MSVC', '14.99.0', 'debug_nonredist'), { recursive: true });
  assert.equal(
    crtFolderIn(installation),
    path.join(installation, 'VC', 'Redist', 'MSVC', '14.44.35112', 'x64', 'Microsoft.VC143.CRT'),
  );
  assert.equal(crtFolderIn(path.join(installation, 'missing')), undefined);
});
