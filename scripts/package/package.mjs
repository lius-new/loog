#!/usr/bin/env node
// Loom packaging entry point.
//
//   node scripts/package/package.mjs <step> [options]
//
// Steps run in this order; `all` runs every step:
//   setup    Install the packaging toolchain into .packaging/tools
//   compile  Build the release binary with Cargo
//   package  Stage the application folder and write the portable archive
//   bundle   Build the installers (Windows setup, Linux .deb and AppImage,
//            macOS .dmg)
//   manifest Write the signed update manifest (see manifest.mjs); `all`
//            skips it when no signing key is set
//
// `keygen` creates a new update signing key pair.
//
// Options:
//   --platform <name>         windows, macos or linux (default: the current OS)
//   --expect-version <x.y.z>  Fail unless Cargo.toml has this version
//
// Downloads, tools, staging folders and final artifacts all live under the
// git-ignored `.packaging/` directory; nothing is installed system-wide.

import path from 'node:path';

import * as linux from './linux/index.mjs';
import * as macos from './macos/index.mjs';
import * as windows from './windows/index.mjs';
import { keygen, writeManifest } from './manifest.mjs';
import { ensureLocked } from './lib/lock.mjs';
import { DIST_DIR, PackagingError, ROOT, cargoPackageField, fail } from './lib/util.mjs';

const STEPS = ['setup', 'compile', 'package', 'bundle', 'manifest'];

const PLATFORMS = {
  windows: {
    setup: windows.setup,
    compile: windows.compile,
    package: windows.packageApp,
    bundle: windows.bundle,
  },
  linux: {
    setup: linux.setup,
    compile: linux.compile,
    package: linux.packageApp,
    bundle: linux.bundle,
  },
  macos: {
    setup: macos.setup,
    compile: macos.compile,
    package: macos.packageApp,
    bundle: macos.bundle,
  },
};

function usage() {
  console.log(`Usage: node scripts/package/package.mjs <${[...STEPS, 'all', 'keygen'].join('|')}> [options]

Options:
  --platform <name>         windows, macos or linux (default: the current OS)
  --expect-version <x.y.z>  Fail unless Cargo.toml has this version
  -h, --help                Show this help`);
}

function parseArguments(argv) {
  const options = { step: undefined, platform: defaultPlatform(), expectVersion: undefined };
  for (let index = 0; index < argv.length; index += 1) {
    const argument = argv[index];
    switch (argument) {
      case '-h':
      case '--help':
        usage();
        process.exit(0);
        break;
      case '--platform':
      case '--expect-version': {
        const value = argv[index + 1];
        if (value === undefined || value.startsWith('--')) {
          fail(`${argument} needs a value`);
        }
        index += 1;
        if (argument === '--platform') {
          options.platform = value;
        } else {
          options.expectVersion = value.replace(/^v/, '');
        }
        break;
      }
      default:
        if (argument.startsWith('-') || options.step !== undefined) {
          fail(`Unexpected argument: ${argument}`);
        }
        options.step = argument;
    }
  }
  if (options.step === undefined) {
    usage();
    process.exit(1);
  }
  if (!['all', 'keygen'].includes(options.step) && !STEPS.includes(options.step)) {
    fail(`Unknown step: ${options.step}`);
  }
  return options;
}

function defaultPlatform() {
  return { win32: 'windows', darwin: 'macos', linux: 'linux' }[process.platform];
}

async function main() {
  const [major] = process.versions.node.split('.').map(Number);
  if (major < 20) {
    fail(`Node.js 20 or newer is required (found ${process.versions.node}).`);
  }

  const options = parseArguments(process.argv.slice(2));
  if (options.step === 'keygen') {
    keygen();
    return;
  }
  const platform = PLATFORMS[options.platform];
  if (!platform) {
    fail(`Unknown platform: ${options.platform ?? process.platform}`);
  }

  const version = cargoPackageField('version');
  if (options.expectVersion !== undefined && options.expectVersion !== version) {
    fail(`Requested version ${options.expectVersion} does not match Cargo.toml version ${version}`);
  }

  // Packaged builds check for updates; plain `cargo build` builds do not.
  process.env.LOOM_UPDATES = '1';
  const steps = options.step === 'all' ? STEPS : [options.step];
  // The bundled extensions come from the live registry when no lock was handed over.
  if (steps.includes('package')) await ensureLocked();
  for (const name of steps) {
    if (name === 'manifest') {
      writeManifest({ platform: options.platform, version, required: options.step === 'manifest' });
    } else {
      await platform[name]({ version });
    }
  }
  if (steps.some((name) => ['package', 'bundle', 'manifest'].includes(name))) {
    console.log(`\nArtifacts: ${path.relative(ROOT, DIST_DIR)}`);
  }
}

main().catch((error) => {
  if (error instanceof PackagingError) {
    console.error(`\nerror: ${error.message}`);
  } else {
    console.error(error);
  }
  process.exit(1);
});
