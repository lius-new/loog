import {prepareDependencies} from '../../dependencies/ensure.mjs';
// macOS packaging: a universal (arm64 + x86_64) Loog.app, shipped as a .zip
// and a drag-to-Applications .dmg. Packages use the system Git: Loog falls
// back to the `git` on PATH when no managed runtime is present.
//
// Signing is ad-hoc unless LOOG_MACOS_SIGN_IDENTITY names a Developer ID
// Application identity in the keychain. With LOOG_MACOS_NOTARY_APPLE_ID,
// LOOG_MACOS_NOTARY_TEAM_ID and LOOG_MACOS_NOTARY_PASSWORD (an app-specific
// password) the app and the disk image are also notarized and stapled.

import fs from 'node:fs';
import path from 'node:path';
import { stageBundled } from '../lib/lock.mjs';

import {
  DIST_DIR,
  ROOT,
  WORK_DIR,
  capture,
  fail,
  info,
  run,
  step,
  writeChecksums,
} from '../lib/util.mjs';

const PLATFORM_DIR = path.join(WORK_DIR, 'macos');
const TARGET_NAME = 'macos-universal';
const RUST_TARGETS = ['aarch64-apple-darwin', 'x86_64-apple-darwin'];
const ARCHITECTURES = ['arm64', 'x86_64'];
const BUNDLE_ID = 'io.github.lius-new.loog';
const MINIMUM_SYSTEM_VERSION = '11.0';

/** Files copied from the repository into Contents/Resources. */
const APP_FILES = ['README.md', 'LICENSE-MIT', 'LICENSE-APACHE', 'THIRD_PARTY_NOTICES.md'];

/** iconset entries and the icons/source/app-icon-transparent-<size>.png each uses. */
const ICONSET = [
  ['icon_16x16.png', 16],
  ['icon_16x16@2x.png', 32],
  ['icon_32x32.png', 32],
  ['icon_32x32@2x.png', 64],
  ['icon_128x128.png', 128],
  ['icon_128x128@2x.png', 256],
  ['icon_256x256.png', 256],
  ['icon_256x256@2x.png', 512],
  ['icon_512x512.png', 512],
  ['icon_512x512@2x.png', 1024],
];

/**
 * `Contents/Resources/bin/loog`, the terminal command; link it into PATH,
 * e.g. /usr/local/bin. It follows that link back to the bundle, makes
 * relative paths absolute (the app does not start in the terminal's
 * directory) and starts a new instance like `loog.cmd` on Windows.
 */
const LOOG_COMMAND = `#!/bin/sh
# Open Loog from a terminal: loog, loog ., loog <path>...
self=$0
while [ -L "$self" ]; do
  target=$(readlink "$self")
  case $target in
    /*) self=$target ;;
    *) self=$(dirname "$self")/$target ;;
  esac
done
app=$(cd "$(dirname "$self")/../../.." && pwd -P)
for arg do
  shift
  case $arg in
    /* | -*) ;;
    *) arg=$PWD/$arg ;;
  esac
  set -- "$@" "$arg"
done
exec open -n -a "$app" --args "$@"
`;

const universalBinary = () => path.join(ROOT, 'target', 'universal-apple-darwin', 'release', 'Loog');
const targetBinary = (target) => path.join(ROOT, 'target', target, 'release', 'Loog');
const appBundle = () => path.join(PLATFORM_DIR, 'stage', 'Loog.app');
const zipPath = (version) => path.join(DIST_DIR, `Loog-${version}-${TARGET_NAME}.zip`);
const dmgPath = (version) => path.join(DIST_DIR, `Loog-${version}-${TARGET_NAME}.dmg`);

function requireMacOS() {
  if (process.platform !== 'darwin') {
    fail('macOS packages must be built on macOS.');
  }
}

function requireTools(names) {
  const missing = names.filter(
    (name) => capture('sh', ['-c', `command -v ${name} || true`]) === '',
  );
  if (missing.length > 0) {
    fail(`Missing packaging tools: ${missing.join(', ')} (install the Xcode Command Line Tools).`);
  }
}

/** Signing and notarization settings from the environment. */
function signing() {
  const identity = process.env.LOOG_MACOS_SIGN_IDENTITY || '';
  const notary = {
    appleId: process.env.LOOG_MACOS_NOTARY_APPLE_ID || '',
    teamId: process.env.LOOG_MACOS_NOTARY_TEAM_ID || '',
    password: process.env.LOOG_MACOS_NOTARY_PASSWORD || '',
  };
  const notarySet = Object.values(notary).filter(Boolean).length;
  if (notarySet !== 0 && notarySet !== 3) {
    fail('Set all of LOOG_MACOS_NOTARY_APPLE_ID, _TEAM_ID and _PASSWORD, or none.');
  }
  if (notarySet === 3 && !identity) {
    fail('Notarization needs a Developer ID identity in LOOG_MACOS_SIGN_IDENTITY.');
  }
  return { identity, notary: notarySet === 3 ? notary : undefined };
}

/** Check the Xcode tools and install both Rust targets. */
export function setup() {
  requireMacOS();
  step('Checking Xcode command line tools');
  requireTools(['lipo', 'codesign', 'ditto', 'hdiutil', 'iconutil', 'plutil', 'otool', 'xcrun']);
  const { identity, notary } = signing();
  info(`Signing: ${identity ? identity : 'ad-hoc'}${notary ? ', notarized' : ''}`);

  step('Installing Rust targets');
  run('rustup', ['target', 'add', ...RUST_TARGETS]);
}

/** Build Loog for both architectures and merge them with lipo. */
export async function compile() {
  requireMacOS();
  // Pin the deployment target so the binary's minimum macOS matches
  // LSMinimumSystemVersion whatever the build machine runs.
  const env = { ...process.env, MACOSX_DEPLOYMENT_TARGET: MINIMUM_SYSTEM_VERSION };
  for (const target of RUST_TARGETS) {
    step(`Compiling Loog (release, ${target})`);
    run('cargo', ['build', '--release', '--locked', '--target', target], { env: (await prepareDependencies({target, env})).env });
  }
  step('Creating the universal binary');
  fs.mkdirSync(path.dirname(universalBinary()), { recursive: true });
  run('lipo', ['-create', '-output', universalBinary(), ...RUST_TARGETS.map(targetBinary)]);
}

/**
 * Assemble and sign Loog.app, check it and write the .zip. Notarization
 * happens in `bundle`, which rewrites the .zip with the stapled app.
 */
export function packageApp({ version }) {
  requireMacOS();
  const binary = universalBinary();
  if (!fs.existsSync(binary)) {
    fail('The universal binary is missing. Run the `compile` step first.');
  }
  const { identity } = signing();

  const app = appBundle();
  step(`Staging ${path.relative(ROOT, app)}`);
  fs.rmSync(path.join(PLATFORM_DIR, 'stage'), { recursive: true, force: true });
  const contents = path.join(app, 'Contents');
  const resources = path.join(contents, 'Resources');
  fs.mkdirSync(path.join(contents, 'MacOS'), { recursive: true });
  fs.mkdirSync(path.join(resources, 'bin'), { recursive: true });
  fs.copyFileSync(binary, path.join(contents, 'MacOS', 'Loog'));
  fs.chmodSync(path.join(contents, 'MacOS', 'Loog'), 0o755);
  fs.writeFileSync(path.join(contents, 'Info.plist'), infoPlist(version));
  fs.writeFileSync(path.join(contents, 'PkgInfo'), 'APPL????');
  fs.writeFileSync(path.join(resources, 'bin', 'loog'), LOOG_COMMAND);
  fs.chmodSync(path.join(resources, 'bin', 'loog'), 0o755);
  for (const file of APP_FILES) {
    fs.copyFileSync(path.join(ROOT, file), path.join(resources, file));
  }
  writeIcon(path.join(resources, 'Loog.icns'));
  // Grammars are WebAssembly, which is data to the system: nothing to sign.
  stageBundled({ destination: path.join(resources, 'extensions') });

  sign(app, identity, path.join(ROOT, 'scripts/package/macos/entitlements.plist'));
  checkApp(app);

  step('Creating .zip');
  zipApp(app, zipPath(version));
  info(path.relative(ROOT, zipPath(version)));
}

function infoPlist(version) {
  const entries = {
    CFBundleDevelopmentRegion: 'en',
    CFBundleDisplayName: 'Loog',
    CFBundleExecutable: 'Loog',
    CFBundleIconFile: 'Loog',
    CFBundleIdentifier: BUNDLE_ID,
    CFBundleInfoDictionaryVersion: '6.0',
    CFBundleName: 'Loog',
    CFBundlePackageType: 'APPL',
    CFBundleShortVersionString: version,
    // Must be numeric: pre-release and build suffixes are dropped.
    CFBundleVersion: version.split(/[-+]/)[0],
    LSApplicationCategoryType: 'public.app-category.developer-tools',
    LSMinimumSystemVersion: MINIMUM_SYSTEM_VERSION,
    NSHighResolutionCapable: true,
    NSSupportsAutomaticGraphicsSwitching: true,
  };
  const escape = (value) =>
    value.replaceAll('&', '&amp;').replaceAll('<', '&lt;').replaceAll('>', '&gt;');
  const body = Object.entries(entries)
    .map(([key, value]) =>
      typeof value === 'boolean'
        ? `\t<key>${key}</key>\n\t<${value}/>`
        : `\t<key>${key}</key>\n\t<string>${escape(value)}</string>`,
    )
    .join('\n');
  return `<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
${body}
</dict>
</plist>
`;
}

function writeIcon(output) {
  const iconset = path.join(PLATFORM_DIR, 'Loog.iconset');
  fs.rmSync(iconset, { recursive: true, force: true });
  fs.mkdirSync(iconset, { recursive: true });
  for (const [name, size] of ICONSET) {
    const source = path.join(ROOT, 'icons', 'source', `app-icon-transparent-${size}.png`);
    if (!fs.existsSync(source)) {
      fail(`Missing application icon: ${path.relative(ROOT, source)}`);
    }
    fs.copyFileSync(source, path.join(iconset, name));
  }
  run('iconutil', ['-c', 'icns', iconset, '-o', output]);
  fs.rmSync(iconset, { recursive: true, force: true });
}

/**
 * Developer ID builds use the hardened runtime and a secure timestamp, which
 * notarization requires. Ad-hoc signing still matters: arm64 refuses to run
 * unsigned code, and the bundle needs a seal over its resources.
 */
function sign(target, identity, entitlements) {
  step(`Signing ${path.basename(target)} (${identity || 'ad-hoc'})`);
  const args = identity
    ? ['--force', '--timestamp', '--options', 'runtime', '--sign', identity]
    : ['--force', '--sign', '-'];
  if (entitlements) args.push('--entitlements', entitlements);
  args.push(target);
  run('codesign', args);
}

/**
 * The GUI cannot be exercised here, so check what can be: both slices are
 * present, the signature and Info.plist are valid, and the binary links only
 * system libraries (a Homebrew dylib would break on other Macs).
 */
function checkApp(app) {
  step('Checking Loog.app');
  const binary = path.join(app, 'Contents', 'MacOS', 'Loog');
  const architectures = capture('lipo', ['-archs', binary]).split(/\s+/);
  const missing = ARCHITECTURES.filter((name) => !architectures.includes(name));
  if (missing.length > 0) {
    fail(`Loog.app is missing architectures: ${missing.join(', ')}`);
  }
  info(`Architectures: ${architectures.join(', ')}`);
  capture('plutil', ['-lint', path.join(app, 'Contents', 'Info.plist')]);
  run('codesign', ['--verify', '--deep', '--strict', '--verbose=2', app]);

  // Skip the per-architecture header lines (`<binary> (architecture arm64):`).
  const libraries = capture('otool', ['-L', '-arch', 'all', binary])
    .split('\n')
    .filter((line) => !line.trimEnd().endsWith(':'))
    .map((line) => line.trim().split(' (')[0])
    .filter((line) => line.startsWith('/') || line.startsWith('@'));
  const foreign = [...new Set(libraries)].filter(
    (library) => !library.startsWith('/usr/lib/') && !library.startsWith('/System/Library/'),
  );
  if (foreign.length > 0) {
    fail(`Loog links libraries outside macOS:\n${foreign.join('\n')}`);
  }
  info(`Links ${new Set(libraries).size} system libraries`);
  capture('sh', ['-n', path.join(app, 'Contents', 'Resources', 'bin', 'loog')]);
}

/** ditto keeps the bundle's symlinks, permissions and signature intact. */
function zipApp(app, output) {
  fs.mkdirSync(DIST_DIR, { recursive: true });
  fs.rmSync(output, { force: true });
  run('ditto', ['-c', '-k', '--sequesterRsrc', '--keepParent', app, output]);
}

/**
 * Notarize and staple the app (when configured), rewrite the .zip with the
 * stapled app, build the .dmg and write SHA256SUMS.
 */
export function bundle({ version }) {
  requireMacOS();
  const app = appBundle();
  if (!fs.existsSync(path.join(app, 'Contents', 'MacOS', 'Loog'))) {
    fail('The staged application is missing. Run the `package` step first.');
  }
  const { identity, notary } = signing();

  if (notary) {
    notarize(zipPath(version), notary);
    run('xcrun', ['stapler', 'staple', app]);
    step('Rewriting .zip with the stapled app');
    zipApp(app, zipPath(version));
  }

  const dmg = dmgPath(version);
  buildDmg(app, version, dmg);
  if (identity) {
    sign(dmg, identity);
  }
  if (notary) {
    notarize(dmg, notary);
    run('xcrun', ['stapler', 'staple', dmg]);
  }

  const prefix = `Loog-${version}-${TARGET_NAME}`;
  const checksums = path.join(DIST_DIR, `${prefix}-SHA256SUMS.txt`);
  const outputs = [zipPath(version), dmg].filter((file) => fs.existsSync(file));
  writeChecksums(outputs, checksums);
  for (const output of [...outputs, checksums]) {
    info(path.relative(ROOT, output));
  }
}

/** A compressed disk image with Loog.app next to an Applications link. */
function buildDmg(app, version, output) {
  step('Building the .dmg');
  const source = path.join(PLATFORM_DIR, 'dmg');
  fs.rmSync(source, { recursive: true, force: true });
  fs.mkdirSync(source, { recursive: true });
  run('ditto', [app, path.join(source, 'Loog.app')]);
  fs.symlinkSync('/Applications', path.join(source, 'Applications'));
  fs.rmSync(output, { force: true });
  run('hdiutil', [
    'create',
    '-volname',
    `Loog ${version}`,
    '-srcfolder',
    source,
    '-fs',
    'HFS+',
    '-format',
    'UDZO',
    '-ov',
    output,
  ]);
  fs.rmSync(source, { recursive: true, force: true });
}

/** Submit to Apple's notary service and wait; print the log when rejected. */
function notarize(file, { appleId, teamId, password }) {
  step(`Notarizing ${path.basename(file)}`);
  const credentials = ['--apple-id', appleId, '--team-id', teamId, '--password', password];
  const result = JSON.parse(
    capture('xcrun', [
      'notarytool',
      'submit',
      file,
      ...credentials,
      '--wait',
      '--output-format',
      'json',
    ]),
  );
  info(`Submission ${result.id}: ${result.status}`);
  if (result.status !== 'Accepted') {
    console.log(capture('xcrun', ['notarytool', 'log', result.id, ...credentials]));
    fail(`Notarization of ${path.basename(file)} ended with status ${result.status}`);
  }
}
