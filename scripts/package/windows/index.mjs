import {prepareDependencies} from '../../dependencies/ensure.mjs';
// Windows packaging: a per-user Inno Setup installer and a portable zip, both
// carrying Loom.exe, the `loom` shell command and a managed MinGit runtime
// under `runtime/git`.

import fs from 'node:fs';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { stageBundled } from '../lib/lock.mjs';
import { stageLspServers } from '../lib/lsp-servers.mjs';

import {
  DIST_DIR,
  ROOT,
  TOOLS_DIR,
  WORK_DIR,
  capture,
  cargoPackageField,
  createZip,
  download,
  extractZipOnce,
  fail,
  info,
  listFiles,
  run,
  sha256File,
  step,
  writeChecksums,
} from '../lib/util.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const PLATFORM_DIR = path.join(WORK_DIR, 'windows');
const TARGET_NAME = 'windows-x86_64';

// Pinned third-party inputs. To upgrade, change the version, URL and hash
// together; the hashes are the GitHub release asset digests.
const INNO_SETUP = {
  version: '7.1.0',
  url: 'https://github.com/jrsoftware/issrc/releases/download/is-7_1_0/innosetup-7.1.0-x64.exe',
  file: 'innosetup-7.1.0-x64.exe',
  sha256: '0362a383ed217d4c4239b5933866dd96d3eb2102737da92f80f6057a4b40df2f',
};

const MINGIT = {
  version: '2.56.0.2',
  tag: 'v2.56.0.windows.2',
  url: 'https://github.com/git-for-windows/git/releases/download/v2.56.0.windows.2/MinGit-2.56.0.2-64-bit.zip',
  file: 'MinGit-2.56.0.2-64-bit.zip',
  sha256: 'da35e72aa21c005a5a0d298cfbae110bc1609a815730ea0dde84b01a1b3cd3be',
};

/** Top-level files copied from the repository into the application folder. */
const APP_FILES = ['README.md', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD_PARTY_NOTICES.md'];

/**
 * `bin\loom.cmd`, the command the installer's "Add to PATH" task exposes.
 * `start` detaches the GUI process so the terminal returns immediately;
 * relative paths resolve against the terminal's directory.
 */
const LOOM_CMD = [
  '@echo off',
  'rem Open Loom from a terminal: loom, loom ., loom <path>...',
  'start "" "%~dp0..\\Loom.exe" %*',
  '',
].join('\r\n');

const innoDir = () => path.join(TOOLS_DIR, `innosetup-${INNO_SETUP.version}`);
const minGitDir = () => path.join(TOOLS_DIR, `mingit-${MINGIT.version}`);
const releaseExe = () => path.join(ROOT, 'target', 'release', 'Loom.exe');
const stageDir = (version) => path.join(PLATFORM_DIR, 'stage', `Loom-${version}-${TARGET_NAME}`);

function requireWindows() {
  if (process.platform !== 'win32') {
    fail('Windows packages must be built on Windows.');
  }
}

/** Install Inno Setup and fetch MinGit into `.packaging/tools`. */
export async function setup() {
  requireWindows();
  step(`Installing Inno Setup ${INNO_SETUP.version}`);
  const installer = await download(INNO_SETUP);
  installInnoSetup(installer);
  for (const required of ['ISCC.exe', 'Default.isl']) {
    if (!fs.existsSync(path.join(innoDir(), required))) {
      fail(`Inno Setup is missing ${required}: ${innoDir()}`);
    }
  }

  step(`Fetching MinGit ${MINGIT.version}`);
  const minGitArchive = await download(MINGIT);
  extractZipOnce(minGitArchive, minGitDir());
  if (!fs.existsSync(path.join(minGitDir(), 'cmd', 'git.exe'))) {
    fail(`MinGit archive has no cmd/git.exe: ${minGitDir()}`);
  }
}

/**
 * Inno Setup's own installer has a portable mode (`/PORTABLE=1`): no
 * uninstaller, registry entries, shortcuts or file associations. Combined with
 * a per-user, very silent install it only writes the target folder. It goes
 * to a temporary folder first so an interrupted install never looks complete.
 */
function installInnoSetup(installer) {
  if (fs.existsSync(path.join(innoDir(), 'ISCC.exe'))) {
    info(`Using installed ${path.relative(ROOT, innoDir())}`);
    return;
  }
  fs.mkdirSync(TOOLS_DIR, { recursive: true });
  const temporary = `${innoDir()}.partial`;
  fs.rmSync(temporary, { recursive: true, force: true });
  fs.rmSync(innoDir(), { recursive: true, force: true });
  run(installer, [
    '/PORTABLE=1',
    '/CURRENTUSER',
    '/VERYSILENT',
    '/SUPPRESSMSGBOXES',
    '/NORESTART',
    '/SP-',
    `/DIR=${temporary}`,
    `/LOG=${path.join(TOOLS_DIR, `innosetup-${INNO_SETUP.version}-install.log`)}`,
  ]);
  fs.renameSync(temporary, innoDir());
}

function iscc() {
  const executable = path.join(innoDir(), 'ISCC.exe');
  if (!fs.existsSync(executable)) {
    fail('Inno Setup is not installed. Run the `setup` step first.');
  }
  return executable;
}

/** Build the optimized Loom.exe. */
export async function compile() {
  requireWindows();
  step('Compiling Loom (release)');
  run('cargo', ['build', '--release', '--locked'], {env: (await prepareDependencies()).env});
}

/**
 * Assemble the application folder, embed the managed Git runtime with its
 * manifest, smoke-test that runtime and write the portable zip.
 */
export function packageApp({ version }) {
  requireWindows();
  const exe = releaseExe();
  if (!fs.existsSync(exe)) {
    fail('target/release/Loom.exe is missing. Run the `compile` step first.');
  }
  if (!fs.existsSync(minGitDir())) {
    fail('MinGit is missing. Run the `setup` step first.');
  }

  const stage = stageDir(version);
  step(`Staging ${path.relative(ROOT, stage)}`);
  fs.rmSync(path.join(PLATFORM_DIR, 'stage'), { recursive: true, force: true });
  fs.mkdirSync(path.join(stage, 'bin'), { recursive: true });
  fs.copyFileSync(exe, path.join(stage, 'Loom.exe'));
  stageBundled({ destination: path.join(stage, 'extensions') });
  stageLspServers({ destination: path.join(stage, 'lsp-servers') });
  fs.writeFileSync(path.join(stage, 'bin', 'loom.cmd'), LOOM_CMD);
  for (const file of APP_FILES) {
    fs.copyFileSync(path.join(ROOT, file), path.join(stage, file));
  }

  step(`Embedding MinGit ${MINGIT.version}`);
  const runtime = path.join(stage, 'runtime', 'git');
  fs.cpSync(minGitDir(), runtime, { recursive: true });
  writeRuntimeMetadata(runtime);
  smokeTestRuntime(runtime);

  step('Creating portable zip');
  fs.mkdirSync(DIST_DIR, { recursive: true });
  const archive = path.join(DIST_DIR, `Loom-${version}-${TARGET_NAME}-portable.zip`);
  createZip(stage, archive);
  info(path.relative(ROOT, archive));
}

/**
 * Git is GPL-2.0: keep its license, upstream version, download URL and a
 * source pointer next to the binaries (see THIRD_PARTY_NOTICES.md). The
 * manifest is what Loom checks before trusting the managed runtime.
 */
function writeRuntimeMetadata(runtime) {
  fs.writeFileSync(path.join(runtime, 'VERSION'), `${MINGIT.version}\n`);
  fs.writeFileSync(
    path.join(runtime, 'SOURCE.txt'),
    [
      `Git for Windows (MinGit) ${MINGIT.version}`,
      `Binary: ${MINGIT.url}`,
      `SHA-256: ${MINGIT.sha256}`,
      `Source: https://github.com/git-for-windows/git/tree/${MINGIT.tag}`,
      'License: GNU General Public License version 2 (see LICENSE.txt)',
      '',
    ].join('\n'),
  );
  if (!fs.existsSync(path.join(runtime, 'LICENSE.txt'))) {
    fail('MinGit LICENSE.txt is missing; refusing to ship Git without its license.');
  }

  const files = listFiles(runtime)
    .filter((file) => file !== 'MANIFEST.json')
    .map((file) => ({ path: file, sha256: sha256File(path.join(runtime, file)) }));
  const manifest = {
    version: MINGIT.version,
    platform: 'windows',
    architecture: 'x86_64',
    source: MINGIT.url,
    generated_at: new Date().toISOString(),
    files,
  };
  fs.writeFileSync(path.join(runtime, 'MANIFEST.json'), `${JSON.stringify(manifest, null, 2)}\n`);
  info(`MANIFEST.json covers ${files.length} files`);
}

function smokeTestRuntime(runtime) {
  step('Smoke-testing the embedded Git runtime');
  const git = path.join(runtime, 'cmd', 'git.exe');
  const scratch = path.join(PLATFORM_DIR, 'git-smoke');
  fs.rmSync(scratch, { recursive: true, force: true });
  fs.mkdirSync(scratch, { recursive: true });
  // Keep the user's and the machine's Git configuration out of the test.
  const env = { ...process.env, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: 'NUL' };
  info(capture(git, ['--version'], { env }));
  capture(git, ['-C', scratch, 'init', '--quiet'], { env });
  capture(git, ['-C', scratch, 'status', '--porcelain=v2'], { env });
  fs.rmSync(scratch, { recursive: true, force: true });
}

/** Compile the Inno Setup installer from the staged application folder. */
export function bundle({ version }) {
  requireWindows();
  const stage = stageDir(version);
  if (!fs.existsSync(path.join(stage, 'Loom.exe'))) {
    fail('The staged application is missing. Run the `package` step first.');
  }

  step('Building the installer');
  fs.mkdirSync(DIST_DIR, { recursive: true });
  const prefix = `Loom-${version}-${TARGET_NAME}`;
  run(iscc(), [
    '/Qp',
    `/DVersion=${version}`,
    `/DVersionQuad=${versionQuad(version)}`,
    `/DStageDir=${stage}`,
    `/DOutputDir=${DIST_DIR}`,
    `/DOutputBase=${prefix}-setup`,
    `/DAppIcon=${path.join(ROOT, 'icons', 'app.ico')}`,
    `/DHomepage=${cargoPackageField('repository')}`,
    path.join(HERE, 'installer.iss'),
  ]);

  const checksums = path.join(DIST_DIR, `${prefix}-SHA256SUMS.txt`);
  const outputs = [`${prefix}-portable.zip`, `${prefix}-setup.exe`]
    .map((name) => path.join(DIST_DIR, name))
    .filter((file) => fs.existsSync(file));
  writeChecksums(outputs, checksums);
  for (const output of [...outputs, checksums]) {
    info(path.relative(ROOT, output));
  }
}

/** Windows version resources need four numeric parts; pre-release and build
 * suffixes (`-beta.1`, `+abc`) are dropped. */
function versionQuad(version) {
  const parts = version
    .split(/[-+]/)[0]
    .split('.')
    .map((part) => Number.parseInt(part, 10) || 0);
  while (parts.length < 4) {
    parts.push(0);
  }
  return parts.slice(0, 4).join('.');
}
