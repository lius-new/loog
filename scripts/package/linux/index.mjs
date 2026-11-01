import {prepareDependencies} from '../../dependencies/ensure.mjs';
// Linux packaging: a portable tar.gz with a per-user install script, a .deb
// and an AppImage, all built from one staged application folder. Linux
// packages use the system Git (the .deb depends on `git`); Loom falls back to
// the Git on PATH when no managed runtime is present.

import fs from 'node:fs';
import path from 'node:path';
import { stageBundled } from '../lib/lock.mjs';
import { stageLspServers } from '../lib/lsp-servers.mjs';

import {
  DIST_DIR,
  ROOT,
  TOOLS_DIR,
  WORK_DIR,
  capture,
  cargoPackageField,
  createTarGz,
  download,
  fail,
  info,
  run,
  step,
  writeChecksums,
} from '../lib/util.mjs';

const PLATFORM_DIR = path.join(WORK_DIR, 'linux');
const TARGET_NAME = 'linux-x86_64';
const DEB_ARCHITECTURE = 'amd64';
const DEB_MAINTAINER = 'Loom maintainers <lius-new@users.noreply.github.com>';

// Pinned third-party inputs. To upgrade, change the version, URL and hash
// together; the hashes are the GitHub release asset digests.
const APPIMAGETOOL = {
  version: '1.9.1',
  url: 'https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage',
  file: 'appimagetool-1.9.1-x86_64.AppImage',
  sha256: 'ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0',
};

// Passed to appimagetool explicitly so the build never fetches an unpinned
// runtime. The type 2 runtime is static and works without libfuse2.
const APPIMAGE_RUNTIME = {
  version: '20251108',
  url: 'https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64',
  file: 'appimage-runtime-20251108-x86_64',
  sha256: '2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d',
};

/** Top-level files copied from the repository into the application folder. */
const APP_FILES = ['README.md', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD_PARTY_NOTICES.md'];

/** hicolor sizes taken from icons/source/app-icon-transparent-<size>.png. */
const ICON_SIZES = [16, 32, 48, 64, 128, 256, 512];

/**
 * Libraries winit opens with dlopen at runtime, so dpkg-shlibdeps cannot see
 * them. X11 and Wayland are alternatives; a desktop session has one of them.
 */
const DEB_DEPENDS = ['git', 'libxkbcommon0'];
const DEB_RECOMMENDS = [
  'libx11-6',
  'libx11-xcb1',
  'libxcursor1',
  'libxi6',
  'libxrandr2',
  'libxkbcommon-x11-0',
  'libwayland-client0',
  'libwayland-cursor0',
  // File dialogs go through the desktop portal.
  'xdg-desktop-portal',
];

/**
 * `bin/loom`, the terminal command. It resolves symlinks (the .deb links it
 * from /usr/bin) and detaches the GUI so the terminal returns immediately;
 * relative paths resolve against the terminal's directory.
 */
const LOOM_COMMAND = `#!/bin/sh
# Open Loom from a terminal: loom, loom ., loom <path>...
here=$(dirname "$(readlink -f "$0")")
nohup "$here/../Loom" "$@" >/dev/null 2>&1 &
`;

/**
 * `install.sh` in the portable archive: copies the folder to
 * ~/.local/lib/loom and adds the \`loom\` command, menu entry and icons for the
 * current user. \`--uninstall\` removes exactly those, never user data.
 */
const INSTALL_SCRIPT = `#!/bin/sh
# Install Loom for the current user, or remove it with --uninstall.
set -eu

data_home=\${XDG_DATA_HOME:-$HOME/.local/share}
app_dir=$HOME/.local/lib/loom
bin_link=$HOME/.local/bin/loom
desktop_file=$data_home/applications/loom.desktop
icon_sizes="${ICON_SIZES.join(' ')}"

refresh() {
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database -q "$data_home/applications" || true
  fi
  if command -v gtk-update-icon-cache >/dev/null 2>&1; then
    gtk-update-icon-cache -q -t "$data_home/icons/hicolor" || true
  fi
}

if [ "\${1:-}" = "--uninstall" ]; then
  rm -rf "$app_dir"
  if [ -L "$bin_link" ]; then rm -f "$bin_link"; fi
  rm -f "$desktop_file"
  for size in $icon_sizes; do
    rm -f "$data_home/icons/hicolor/\${size}x\${size}/apps/loom.png"
  done
  refresh
  echo "Loom has been removed."
  exit 0
fi

source_dir=$(dirname "$(readlink -f "$0")")
if [ "$source_dir" != "$app_dir" ]; then
  rm -rf "$app_dir.partial"
  mkdir -p "$(dirname "$app_dir")"
  cp -R "$source_dir" "$app_dir.partial"
  rm -rf "$app_dir"
  mv "$app_dir.partial" "$app_dir"
fi

mkdir -p "$(dirname "$bin_link")" "$data_home/applications"
ln -sf "$app_dir/bin/loom" "$bin_link"
sed "s|^Exec=.*|Exec=$app_dir/Loom %F|" "$app_dir/share/applications/loom.desktop" > "$desktop_file"
for size in $icon_sizes; do
  mkdir -p "$data_home/icons/hicolor/\${size}x\${size}/apps"
  cp "$app_dir/share/icons/hicolor/\${size}x\${size}/apps/loom.png" \\
    "$data_home/icons/hicolor/\${size}x\${size}/apps/loom.png"
done
refresh

echo "Loom is installed in $app_dir."
case ":$PATH:" in
  *":$HOME/.local/bin:"*) ;;
  *) echo "Add $HOME/.local/bin to PATH to use the loom command." ;;
esac
`;

/** The menu entry; \`exec\` is the launcher each package installs. */
function desktopEntry(exec) {
  return [
    '[Desktop Entry]',
    'Type=Application',
    'Name=Loom',
    'GenericName=Text Editor',
    `Comment=${cargoPackageField('description')}`,
    `Exec=${exec} %F`,
    'Icon=loom',
    'Terminal=false',
    'Categories=Development;TextEditor;Utility;',
    'MimeType=text/plain;inode/directory;',
    'Keywords=editor;text;code;',
    'StartupNotify=true',
    'StartupWMClass=Loom',
    '',
  ].join('\n');
}

const appImageToolDir = () => path.join(TOOLS_DIR, `appimagetool-${APPIMAGETOOL.version}`);
const releaseBinary = () => path.join(ROOT, 'target', 'release', 'Loom');
const stageDir = (version) => path.join(PLATFORM_DIR, 'stage', `Loom-${version}-${TARGET_NAME}`);
const runtimeFile = () => path.join(WORK_DIR, 'downloads', APPIMAGE_RUNTIME.file);

function requireLinux() {
  if (process.platform !== 'linux' || process.arch !== 'x64') {
    fail('Linux packages must be built on x86_64 Linux.');
  }
}

function requireTools(names) {
  const missing = names.filter(
    (name) => capture('sh', ['-c', `command -v ${name} || true`]) === '',
  );
  if (missing.length > 0) {
    fail(`Missing packaging tools: ${missing.join(', ')} (install dpkg-dev).`);
  }
}

/** Check the system packaging tools and fetch appimagetool and its runtime. */
export async function setup() {
  requireLinux();
  step('Checking system packaging tools');
  requireTools(['tar', 'dpkg-deb', 'dpkg-shlibdeps', 'ldd']);

  step(`Fetching appimagetool ${APPIMAGETOOL.version}`);
  const tool = await download(APPIMAGETOOL);
  extractAppImageOnce(tool, appImageToolDir());

  step(`Fetching AppImage runtime ${APPIMAGE_RUNTIME.version}`);
  await download(APPIMAGE_RUNTIME);
}

/**
 * Run appimagetool from its extracted contents: CI runners have no FUSE, so
 * the AppImage itself cannot mount. Extraction goes to a temporary sibling
 * and is renamed into place, like the other tools.
 */
function extractAppImageOnce(appImage, destination) {
  if (fs.existsSync(path.join(destination, 'AppRun'))) {
    info(`Using extracted ${path.relative(ROOT, destination)}`);
    return;
  }
  const temporary = `${destination}.partial`;
  fs.rmSync(temporary, { recursive: true, force: true });
  fs.rmSync(destination, { recursive: true, force: true });
  fs.mkdirSync(temporary, { recursive: true });
  const executable = path.join(temporary, path.basename(appImage));
  fs.copyFileSync(appImage, executable);
  fs.chmodSync(executable, 0o755);
  run(executable, ['--appimage-extract'], { cwd: temporary, stdio: ['ignore', 'ignore', 'inherit'] });
  fs.renameSync(path.join(temporary, 'squashfs-root'), destination);
  fs.rmSync(temporary, { recursive: true, force: true });
}

function appImageTool() {
  const executable = path.join(appImageToolDir(), 'AppRun');
  if (!fs.existsSync(executable) || !fs.existsSync(runtimeFile())) {
    fail('appimagetool is not installed. Run the `setup` step first.');
  }
  return executable;
}

/** Build the optimized Loom binary. */
export async function compile() {
  requireLinux();
  step('Compiling Loom (release)');
  run('cargo', ['build', '--release', '--locked'], {env: (await prepareDependencies()).env});
}

/**
 * Assemble the application folder (binary, `loom` command, menu entry, icons,
 * install script), check that the binary's libraries resolve and write the
 * portable tar.gz.
 */
export function packageApp({ version }) {
  requireLinux();
  const binary = releaseBinary();
  if (!fs.existsSync(binary)) {
    fail('target/release/Loom is missing. Run the `compile` step first.');
  }

  const stage = stageDir(version);
  step(`Staging ${path.relative(ROOT, stage)}`);
  fs.rmSync(path.join(PLATFORM_DIR, 'stage'), { recursive: true, force: true });
  fs.mkdirSync(path.join(stage, 'bin'), { recursive: true });
  fs.copyFileSync(binary, path.join(stage, 'Loom'));
  stageBundled({ destination: path.join(stage, 'extensions') });
  stageLspServers({ destination: path.join(stage, 'lsp-servers') });
  fs.chmodSync(path.join(stage, 'Loom'), 0o755);
  writeExecutable(path.join(stage, 'bin', 'loom'), LOOM_COMMAND);
  writeExecutable(path.join(stage, 'install.sh'), INSTALL_SCRIPT);
  for (const file of APP_FILES) {
    fs.copyFileSync(path.join(ROOT, file), path.join(stage, file));
  }
  const applications = path.join(stage, 'share', 'applications');
  fs.mkdirSync(applications, { recursive: true });
  // install.sh rewrites Exec to the installed location.
  fs.writeFileSync(path.join(applications, 'loom.desktop'), desktopEntry('Loom'));
  copyIcons(path.join(stage, 'share', 'icons'));

  checkStage(stage);

  step('Creating portable tar.gz');
  fs.mkdirSync(DIST_DIR, { recursive: true });
  const archive = path.join(DIST_DIR, `Loom-${version}-${TARGET_NAME}.tar.gz`);
  createTarGz(stage, archive);
  info(path.relative(ROOT, archive));
}

function writeExecutable(file, contents) {
  fs.writeFileSync(file, contents);
  fs.chmodSync(file, 0o755);
}

function copyIcons(iconsRoot) {
  for (const size of ICON_SIZES) {
    const source = path.join(ROOT, 'icons', 'source', `app-icon-transparent-${size}.png`);
    if (!fs.existsSync(source)) {
      fail(`Missing application icon: ${path.relative(ROOT, source)}`);
    }
    const directory = path.join(iconsRoot, 'hicolor', `${size}x${size}`, 'apps');
    fs.mkdirSync(directory, { recursive: true });
    fs.copyFileSync(source, path.join(directory, 'loom.png'));
  }
}

/**
 * The GUI cannot start on a headless runner, so check what can be checked:
 * every linked library resolves and the shell scripts parse.
 */
function checkStage(stage) {
  step('Checking the staged application');
  const libraries = capture('ldd', [path.join(stage, 'Loom')]);
  const unresolved = libraries.split('\n').filter((line) => line.includes('not found'));
  if (unresolved.length > 0) {
    fail(`Loom links libraries that do not resolve:\n${unresolved.join('\n')}`);
  }
  info(`glibc requirement: ${glibcRequirement(path.join(stage, 'Loom'))}`);
  for (const script of ['bin/loom', 'install.sh']) {
    capture('sh', ['-n', path.join(stage, script)]);
  }
}

/** The newest GLIBC_x.y symbol version the binary references. */
function glibcRequirement(binary) {
  const versions = fs
    .readFileSync(binary, 'latin1')
    .match(/GLIBC_\d+\.\d+(\.\d+)?/g) ?? [];
  const newest = [...new Set(versions)]
    .map((name) => name.slice('GLIBC_'.length))
    .sort((left, right) => compareVersions(right, left))[0];
  return newest ? `glibc ${newest}` : 'unknown';
}

function compareVersions(left, right) {
  const a = left.split('.').map(Number);
  const b = right.split('.').map(Number);
  for (let index = 0; index < Math.max(a.length, b.length); index += 1) {
    const difference = (a[index] ?? 0) - (b[index] ?? 0);
    if (difference !== 0) {
      return difference;
    }
  }
  return 0;
}

/** Build the .deb and the AppImage from the staged folder and write SHA256SUMS. */
export function bundle({ version }) {
  requireLinux();
  const stage = stageDir(version);
  if (!fs.existsSync(path.join(stage, 'Loom'))) {
    fail('The staged application is missing. Run the `package` step first.');
  }
  fs.mkdirSync(DIST_DIR, { recursive: true });
  const prefix = `Loom-${version}-${TARGET_NAME}`;
  const deb = path.join(DIST_DIR, `${prefix}.deb`);
  const appImage = path.join(DIST_DIR, `${prefix}.AppImage`);
  buildDeb(stage, version, deb);
  buildAppImage(stage, appImage);

  const checksums = path.join(DIST_DIR, `${prefix}-SHA256SUMS.txt`);
  const outputs = [`${prefix}.tar.gz`, `${prefix}.deb`, `${prefix}.AppImage`]
    .map((name) => path.join(DIST_DIR, name))
    .filter((file) => fs.existsSync(file));
  writeChecksums(outputs, checksums);
  for (const output of [...outputs, checksums]) {
    info(path.relative(ROOT, output));
  }
}

/**
 * /opt/loom holds the application folder; /usr/bin/loom, the menu entry and
 * the icons are the only files outside it. Uninstalling never touches user
 * data.
 */
function buildDeb(stage, version, output) {
  step('Building the .deb');
  const root = path.join(PLATFORM_DIR, 'deb', 'root');
  fs.rmSync(path.join(PLATFORM_DIR, 'deb'), { recursive: true, force: true });

  const app = path.join(root, 'opt', 'loom');
  fs.mkdirSync(path.dirname(app), { recursive: true });
  fs.cpSync(stage, app, { recursive: true });
  // The portable layout's install script and share/ belong in /usr instead.
  fs.rmSync(path.join(app, 'install.sh'));
  fs.cpSync(path.join(app, 'share'), path.join(root, 'usr', 'share'), { recursive: true });
  fs.rmSync(path.join(app, 'share'), { recursive: true });
  fs.writeFileSync(
    path.join(root, 'usr', 'share', 'applications', 'loom.desktop'),
    desktopEntry('/opt/loom/Loom'),
  );
  fs.mkdirSync(path.join(root, 'usr', 'bin'), { recursive: true });
  fs.symlinkSync('/opt/loom/bin/loom', path.join(root, 'usr', 'bin', 'loom'));
  const doc = path.join(root, 'usr', 'share', 'doc', 'loom');
  fs.mkdirSync(doc, { recursive: true });
  fs.writeFileSync(path.join(doc, 'copyright'), debianCopyright());

  const control = path.join(root, 'DEBIAN');
  fs.mkdirSync(control);
  fs.writeFileSync(
    path.join(control, 'control'),
    [
      'Package: loom',
      `Version: ${debianVersion(version)}`,
      `Architecture: ${DEB_ARCHITECTURE}`,
      `Maintainer: ${DEB_MAINTAINER}`,
      `Installed-Size: ${installedSizeKiB(root)}`,
      `Depends: ${[...shlibDepends(path.join(app, 'Loom')), ...DEB_DEPENDS].join(', ')}`,
      `Recommends: ${DEB_RECOMMENDS.join(', ')}`,
      'Section: editors',
      'Priority: optional',
      `Homepage: ${cargoPackageField('repository')}`,
      `Description: ${cargoPackageField('description')}`,
      ' Loom is a text editor with an integrated terminal and Git support.',
      '',
    ].join('\n'),
  );
  normalizePermissions(root);
  run('dpkg-deb', ['--root-owner-group', '-Zxz', '--build', root, output]);
}

/**
 * dpkg-shlibdeps resolves the linked libraries to packages. It needs a
 * debian/control in its working directory, so give it a minimal one.
 */
function shlibDepends(binary) {
  const work = path.join(PLATFORM_DIR, 'deb', 'shlibdeps');
  fs.mkdirSync(path.join(work, 'debian'), { recursive: true });
  fs.writeFileSync(
    path.join(work, 'debian', 'control'),
    'Source: loom\n\nPackage: loom\nArchitecture: any\n',
  );
  const output = capture('dpkg-shlibdeps', ['-O', `-e${binary}`], { cwd: work });
  const line = output.split('\n').find((entry) => entry.startsWith('shlibs:Depends='));
  if (!line) {
    fail(`dpkg-shlibdeps reported no dependencies:\n${output}`);
  }
  return line
    .slice('shlibs:Depends='.length)
    .split(',')
    .map((entry) => entry.trim())
    .filter(Boolean);
}

/** Debian sorts `~` before anything, so pre-releases stay below the release. */
function debianVersion(version) {
  return version.replaceAll('-', '~');
}

function installedSizeKiB(root) {
  let bytes = 0;
  const walk = (directory) => {
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      const full = path.join(directory, entry.name);
      if (entry.isDirectory()) {
        walk(full);
      } else if (entry.isFile()) {
        bytes += fs.statSync(full).size;
      }
    }
  };
  walk(root);
  return Math.ceil(bytes / 1024);
}

/** Directories and executables 0755, other files 0644, whatever the umask. */
function normalizePermissions(root) {
  const walk = (directory) => {
    fs.chmodSync(directory, 0o755);
    for (const entry of fs.readdirSync(directory, { withFileTypes: true })) {
      const full = path.join(directory, entry.name);
      if (entry.isDirectory()) {
        walk(full);
      } else if (entry.isFile()) {
        const executable = (fs.statSync(full).mode & 0o111) !== 0;
        fs.chmodSync(full, executable ? 0o755 : 0o644);
      }
    }
  };
  walk(root);
}

function debianCopyright() {
  const read = (file) => fs.readFileSync(path.join(ROOT, file), 'utf8').trimEnd();
  return [
    'Format: https://www.debian.org/doc/packaging-manuals/copyright-format/1.0/',
    'Upstream-Name: Loom',
    `Source: ${cargoPackageField('repository')}`,
    '',
    'Files: *',
    `License: ${cargoPackageField('license')}`,
    '',
    'License: MIT',
    indent(read('LICENSE-MIT')),
    '',
    'License: Apache-2.0',
    indent(read('LICENSE-APACHE')),
    '',
    'Comment: Third-party notices',
    indent(read('THIRD_PARTY_NOTICES.md')),
    '',
  ].join('\n');
}

/** Debian copyright continuation lines: one leading space, `.` for blanks. */
function indent(text) {
  return text
    .split('\n')
    .map((line) => (line.trim() === '' ? ' .' : ` ${line}`))
    .join('\n');
}

/**
 * AppDir: the application folder under usr/lib/loom, with AppRun, the menu
 * entry and the icon at the root as the AppImage format requires.
 */
function buildAppImage(stage, output) {
  step('Building the AppImage');
  const tool = appImageTool();
  const appDir = path.join(PLATFORM_DIR, 'appimage', 'Loom.AppDir');
  fs.rmSync(path.join(PLATFORM_DIR, 'appimage'), { recursive: true, force: true });

  const app = path.join(appDir, 'usr', 'lib', 'loom');
  fs.mkdirSync(path.dirname(app), { recursive: true });
  fs.cpSync(stage, app, { recursive: true });
  fs.rmSync(path.join(app, 'install.sh'));
  fs.cpSync(path.join(app, 'share'), path.join(appDir, 'usr', 'share'), { recursive: true });
  fs.rmSync(path.join(app, 'share'), { recursive: true });
  fs.writeFileSync(
    path.join(appDir, 'usr', 'share', 'applications', 'loom.desktop'),
    desktopEntry('Loom'),
  );
  fs.symlinkSync('usr/share/applications/loom.desktop', path.join(appDir, 'loom.desktop'));
  fs.symlinkSync('usr/share/icons/hicolor/256x256/apps/loom.png', path.join(appDir, 'loom.png'));
  fs.symlinkSync('loom.png', path.join(appDir, '.DirIcon'));
  writeExecutable(
    path.join(appDir, 'AppRun'),
    '#!/bin/sh\nhere=$(dirname "$(readlink -f "$0")")\nexec "$here/usr/lib/loom/Loom" "$@"\n',
  );

  fs.rmSync(output, { force: true });
  run(tool, ['--no-appstream', '--runtime-file', runtimeFile(), appDir, output], {
    env: { ...process.env, ARCH: 'x86_64' },
  });
  fs.chmodSync(output, 0o755);
}
