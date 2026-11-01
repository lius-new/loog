// Scope expensive checks without removing the unconditional three-platform tests.
import fs from 'node:fs';
import path from 'node:path';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';

export const flags = ['tooling', 'evidence', 'wasm', 'standalone', 'package'];
const all = () => Object.fromEntries(flags.map(flag => [flag, true]));
const none = () => Object.fromEntries(flags.map(flag => [flag, false]));
const starts = (file, prefixes) => prefixes.some(prefix => file.startsWith(prefix));
const common = new Set(['Cargo.toml','Cargo.lock','rust-toolchain','rust-toolchain.toml','build.rs','.gitignore','.gitattributes']);
const language = ['src/domain/language','src/features/syntax','src/features/language_service',
  'src/infrastructure/language','src/infrastructure/syntax','src/app/language',
  'assets/language','assets/extensions/','assets/grammars/','assets/servers/','tests/language-runtime/',
  'tests/fixtures/language','scripts/language-packs/'];
const markdown = ['src/app/markdown','src/features/markdown','src/ui/markdown',
  'src/infrastructure/markdown','src/domain/','crates/editor-core/','crates/markdown-tests/',
  'src/features/settings/','src/ui/theme.rs','src/ui/fonts.rs','src/ui/text_rendering.rs',
  'tests/markdown/','assets/','icons/'];

export function classify(files) {
  if (!Array.isArray(files)) return all();
  const plan = none();
  for (const file of files) {
    // Unknown layouts and changes to CI/toolchain policy take the safe full path.
    if (typeof file !== 'string' || !file || file.includes('\\') || file.split('/').includes('..') ||
      common.has(file) || starts(file, ['.github/','.cargo/','vendor/','scripts/ci/','scripts/dependencies/','scripts/lib/']) ||
      !/^(src|crates|examples|tests|scripts|assets|icons|docs)\//.test(file) &&
        !/^(README\.md|LICENSE[^/]*|THIRD_PARTY_NOTICES\.md|CONTRIBUTING\.md|CHANGELOG\.md)$/.test(file)) return all();
    if (starts(file, ['scripts/','tests/','assets/','icons/'])) plan.tooling = true;
    if (starts(file, ['scripts/package/','scripts/package-','scripts/build/','assets/','icons/']) ||
      /^(README\.md|LICENSE[^/]*|THIRD_PARTY_NOTICES\.md)$/.test(file)) plan.package = true;
    if (starts(file, language)) {
      plan.wasm = true;
      plan.standalone = true;
    }
    if (starts(file, markdown) || starts(file, language)) plan.evidence = true;
    if (file === 'src/main.rs') plan.standalone = true;
  }
  return plan;
}

export function selectPlan({eventName, event = {}, files = null}) {
  if (eventName === 'workflow_dispatch') return event.inputs?.full === false || event.inputs?.full === 'false' ? none() : all();
  if (eventName !== 'pull_request' && eventName !== 'push') return all();
  const plan = classify(files);
  // Main releases will build the final packages once in Release. Preview
  // packaging is a PR recipe check or an explicitly requested manual check.
  if (eventName === 'push') plan.package = false;
  return plan;
}

export function changedFiles({eventName, event, cwd = process.cwd(), git = execFileSync, warn = console.warn}) {
  if (eventName === 'workflow_dispatch') return [];
  const base = eventName === 'pull_request' ? event.pull_request?.base?.sha : event.before;
  const head = eventName === 'pull_request' ? event.pull_request?.head?.sha : event.after;
  const valid = value => typeof value === 'string' && /^[a-f0-9]{40}(?:[a-f0-9]{24})?$/i.test(value) && !/^0+$/.test(value);
  if (!valid(base) || !valid(head)) {
    warn('::warning::CI change range is unavailable; selecting all expensive checks.');
    return null;
  }
  try {
    const range = eventName === 'pull_request' ? base + '...' + head : base + '..' + head;
    // No rename folding: both the old/deleted and new paths affect selection.
    return git('git', ['diff','--name-only','--no-renames','-z',range,'--'],
      {cwd, encoding:'utf8', maxBuffer:32 * 1024 * 1024}).split('\0').filter(Boolean);
  } catch {
    warn('::warning::CI change detection failed; selecting all expensive checks.');
    return null;
  }
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  const eventName = process.env.GITHUB_EVENT_NAME;
  const event = JSON.parse(fs.readFileSync(process.env.GITHUB_EVENT_PATH, 'utf8'));
  const files = changedFiles({eventName,event});
  const plan = selectPlan({eventName,event,files});
  const outputs = [...flags.map(flag => flag + '=' + plan[flag]), 'validation=' + Object.values(plan).some(Boolean)].join('\n') + '\n';
  fs.appendFileSync(process.env.GITHUB_OUTPUT, outputs);
  if (process.env.GITHUB_STEP_SUMMARY) fs.appendFileSync(process.env.GITHUB_STEP_SUMMARY,
    '## CI selection\n\nThree-platform host tests, core checks and static checks always run.\n\n' +
    flags.map(flag => '- ' + flag + ': ' + (plan[flag] ? 'run' : 'skip')).join('\n') + '\n');
  console.log(JSON.stringify({event:eventName, files:files?.length ?? 'unavailable', plan}));
}
