// App-local Visual C++ runtime for the Windows packages.
//
// Loog.exe links the Microsoft C++ runtime dynamically (the prebuilt Skia
// archives only support the dynamic CRT), so a machine without the "Visual C++
// Redistributable" cannot start it: Windows reports that MSVCP140.dll is
// missing. Microsoft allows these DLLs to be redistributed with an application
// (app-local deployment), so the packages carry the ones Loog needs next to
// Loog.exe, where the loader finds them before any system copy.

import fs from 'node:fs';
import path from 'node:path';
import { capture, fail } from '../lib/util.mjs';

/** The redistributable runtime libraries: C++ standard library, compiler runtime, OpenMP and concurrency. */
const RUNTIME_DLL = /^(msvcp140|vcruntime140|concrt140|vcomp140)(_[a-z0-9_]+)?\.dll$/i;

/** True for the DLL names the Visual C++ Redistributable provides. */
export function isRuntimeDll(name) {
  return RUNTIME_DLL.test(name);
}

/** Names of the DLLs a PE image imports, including delay-load imports. */
export function peImports(image) {
  const peOffset = image.readUInt32LE(0x3c);
  if (image.toString('latin1', peOffset, peOffset + 4) !== 'PE\0\0') {
    throw new Error('not a PE image');
  }
  const sectionCount = image.readUInt16LE(peOffset + 6);
  const optionalSize = image.readUInt16LE(peOffset + 20);
  const optional = peOffset + 24;
  const magic = image.readUInt16LE(optional);
  // The data directory table starts at 96 (PE32) or 112 (PE32+) bytes in.
  const directories = optional + (magic === 0x20b ? 112 : 96);
  const sections = [];
  for (let index = 0; index < sectionCount; index += 1) {
    const section = optional + optionalSize + index * 40;
    sections.push({
      size: Math.max(image.readUInt32LE(section + 8), image.readUInt32LE(section + 16)),
      virtual: image.readUInt32LE(section + 12),
      raw: image.readUInt32LE(section + 20),
    });
  }
  const offsetOf = (rva) => {
    const section = sections.find((s) => rva >= s.virtual && rva < s.virtual + s.size);
    return section ? rva - section.virtual + section.raw : -1;
  };
  const nameAt = (rva) => {
    const start = offsetOf(rva);
    return image.toString('latin1', start, image.indexOf(0, start));
  };

  const names = [];
  // Import table: 20-byte descriptors ending with a zero name; delay-load
  // table (directory 13): 32-byte descriptors whose name is the second field.
  for (const [directory, stride, nameField] of [[1, 20, 12], [13, 32, 4]]) {
    const rva = image.readUInt32LE(directories + directory * 8);
    if (rva === 0) {
      continue;
    }
    for (let at = offsetOf(rva); image.readUInt32LE(at + nameField) !== 0; at += stride) {
      names.push(nameAt(image.readUInt32LE(at + nameField)));
    }
  }
  return names;
}

/**
 * Every runtime DLL the image needs, directly or through another runtime DLL
 * (msvcp140.dll itself loads vcruntime140.dll and vcruntime140_1.dll).
 * `available` lists the files of the redistributable directory.
 */
export function requiredRuntimeDlls(executable, redistDir) {
  const available = new Map(fs.readdirSync(redistDir).map((name) => [name.toLowerCase(), name]));
  const needed = new Map();
  const queue = [executable];
  while (queue.length > 0) {
    for (const name of peImports(fs.readFileSync(queue.pop()))) {
      if (!isRuntimeDll(name) || needed.has(name.toLowerCase())) {
        continue;
      }
      const file = available.get(name.toLowerCase());
      if (!file) {
        fail(`${name} is required but missing from ${redistDir}`);
      }
      needed.set(name.toLowerCase(), file);
      queue.push(path.join(redistDir, file));
    }
  }
  return [...needed.values()].sort();
}

/** Compare dotted numeric versions such as `14.44.35112`. */
function compareVersions(a, b) {
  const left = a.split('.').map(Number);
  const right = b.split('.').map(Number);
  for (let index = 0; index < Math.max(left.length, right.length); index += 1) {
    const difference = (left[index] ?? 0) - (right[index] ?? 0);
    if (difference !== 0) {
      return difference;
    }
  }
  return 0;
}

/** The x64 folder holding `Microsoft.VC*.CRT` below one `VC\Redist\MSVC\<version>` folder. */
function crtFolder(versionDir) {
  const x64 = path.join(versionDir, 'x64');
  if (!fs.existsSync(x64)) {
    return undefined;
  }
  const crt = fs.readdirSync(x64).find((name) => /^Microsoft\.VC\d+\.CRT$/.test(name));
  return crt ? path.join(x64, crt) : undefined;
}

/** The newest `Microsoft.VC*.CRT` folder below a Visual Studio installation. */
export function crtFolderIn(installation) {
  const redist = path.join(installation, 'VC', 'Redist', 'MSVC');
  if (!fs.existsSync(redist)) {
    return undefined;
  }
  return fs
    .readdirSync(redist)
    .filter((name) => /^\d+(\.\d+)+$/.test(name))
    .sort(compareVersions)
    .reverse()
    .map((name) => crtFolder(path.join(redist, name)))
    .find(Boolean);
}

/**
 * The Visual Studio redistributable folder matching the toolset that built
 * Loog.exe: `LOOG_VC_REDIST_DIR` when set, otherwise the newest Visual Studio
 * (or Build Tools) installation found by vswhere.
 */
export function findRedistDir(env = process.env) {
  if (env.LOOG_VC_REDIST_DIR) {
    if (!fs.existsSync(env.LOOG_VC_REDIST_DIR)) {
      fail(`LOOG_VC_REDIST_DIR does not exist: ${env.LOOG_VC_REDIST_DIR}`);
    }
    return env.LOOG_VC_REDIST_DIR;
  }
  const vswhere = path.join(
    env['ProgramFiles(x86)'] ?? 'C:\\Program Files (x86)',
    'Microsoft Visual Studio',
    'Installer',
    'vswhere.exe',
  );
  if (fs.existsSync(vswhere)) {
    const installations = capture(vswhere, [
      '-products', '*',
      '-requires', 'Microsoft.VisualStudio.Component.VC.Tools.x86.x64',
      '-property', 'installationPath',
    ]).split(/\r?\n/).filter(Boolean);
    for (const installation of installations) {
      const folder = crtFolderIn(installation);
      if (folder) {
        return folder;
      }
    }
  }
  return fail(
    'The Visual C++ runtime DLLs were not found. Install Visual Studio or Build Tools with the C++ toolset, ' +
      'or set LOOG_VC_REDIST_DIR to a Microsoft.VC*.CRT folder (VC\\Redist\\MSVC\\<version>\\x64).',
  );
}

/** Copy the runtime DLLs `executable` needs into `destination` and return their names. */
export function stageRuntime({ executable, destination, redistDir = findRedistDir() }) {
  const names = requiredRuntimeDlls(executable, redistDir);
  for (const name of names) {
    fs.copyFileSync(path.join(redistDir, name), path.join(destination, name));
  }
  return names;
}
