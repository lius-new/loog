// Runs after preparation on every CI host. CMAKE_COMMAND exposes the actual
// executable selected through PATH, so a runner's preinstalled CMake cannot
// accidentally satisfy this check.
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import assert from 'node:assert/strict';
import {execFileSync} from 'node:child_process';
import {prepareDependencies} from './ensure.mjs';

const prepared = await prepareDependencies({offline: true});
const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'loog-managed-cmake-'));
try {
  const script = path.join(directory, 'probe.cmake');
  fs.writeFileSync(script, 'message(STATUS "Managed CMake: ${CMAKE_COMMAND}")\n');
  const output = execFileSync('cmake', ['-P', script], {env:prepared.env, encoding:'utf8'});
  assert.ok(output.replaceAll('\\','/').includes(prepared.cmake.executable.replaceAll('\\','/')), output);
  // Verify the exported Actions environment as well as local child scoping.
  const githubPath = path.join(directory, 'github-path'), githubEnv = path.join(directory, 'github-env');
  execFileSync(process.execPath, [fileURLToPath(new URL('./ensure.mjs', import.meta.url)), '--offline', '--github'], {
    env:{...process.env, GITHUB_PATH:githubPath, GITHUB_ENV:githubEnv}, stdio:'pipe',
  });
  assert.equal(fs.readFileSync(githubPath, 'utf8').trim(), prepared.cmake.bin);
  assert.equal(fs.readFileSync(githubEnv, 'utf8').trim(), `CMAKE=${prepared.cmake.executable}\nSKIA_BINARIES_URL=${prepared.skia.url}`);
  console.log(output.trim());
  console.log(`Verified private CMake, Skia and Actions environment for ${prepared.target}`);
} finally { fs.rmSync(directory, {recursive:true, force:true}); }
