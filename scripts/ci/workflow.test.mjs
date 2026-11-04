import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import {parseDocument} from 'yaml';
import {flags} from './plan.mjs';
import {requiredChecks} from '../release/verify-ci.mjs';
const load = file => {const doc = parseDocument(fs.readFileSync(new URL('../../' + file,import.meta.url),'utf8'));assert.deepEqual(doc.errors,[]);return doc.toJS();};
const ci = load('.github/workflows/ci.yml'), deep = load('.github/workflows/validation.yml'), release = load('.github/workflows/release.yml');
const runs = job => (job.steps || []).map(step => step.run || '').join('\n');
const expr = text => '$' + '{{ ' + text + ' }}';
test('CI keeps main/PR triggers but ordinary and manual defaults do not request full validation', () => {
  assert.deepEqual(ci.on.push.branches,['main']);assert.ok(Object.hasOwn(ci.on,'pull_request'));
  assert.equal(ci.on.workflow_dispatch.inputs.full.default,false);
  assert.equal(ci.on.push.paths,undefined);assert.equal(ci.on.push['paths-ignore'],undefined);
  assert.equal(deep.on.workflow_dispatch.inputs.full.default,true);
});
test('daily CI has no npm install, tool self-tests, evidence, WASM rebuilds or packaging commands', () => {
  const normal = ['changes','static','core','build','result'].map(id => JSON.stringify(ci.jobs[id])).join('\n');
  assert.doesNotMatch(normal,/npm ci|test-tools|run-wasm|production_markdown_evidence|standalone-host|package\.mjs|startup-smoke\.test/);
  assert.doesNotMatch(JSON.stringify(ci.jobs.static),/actions\/dependencies|linux-build-dependencies/);
  assert.match(runs(ci.jobs.static),/cargo fmt --all -- --check/);
  assert.match(runs(ci.jobs.static),/node scripts\/architecture\/check\.mjs/);
  assert.equal(ci.jobs.quality,undefined);
});
test('commit notes are checked before scope selection without a new CI job or dependency install', () => {
  const job = ci.jobs.changes;
  assert.equal(job.steps.find(step => step.uses === 'actions/checkout@v7').with['fetch-depth'], 0);
  const commands = job.steps.find(step => step.id === 'scope').run.split('\n');
  assert.deepEqual(commands, ['node scripts/commit/check.mjs --event', 'node scripts/ci/plan.mjs']);
  assert.doesNotMatch(runs(job), /npm |fetch |curl |gh /);
});
test('three-platform functionality stays unconditional and Clippy shares the Linux test environment', () => {
  const build = ci.jobs.build;
  assert.equal(build.if,undefined);assert.equal(build.strategy['fail-fast'],false);
  assert.deepEqual(build.strategy.matrix.include.map(row => row.os).sort(),['macos-15','ubuntu-24.04','windows-2025']);
  assert.equal(build.steps.find(step => step.name === 'Run tests').run,'cargo test --locked -p loom');
  assert.equal(build.steps.find(step => step.name === 'Run tests').if,undefined);
  assert.equal(build.steps.find(step => step.name === 'Run Clippy').if,"runner.os == 'Linux'");
  for (const crate of ['loom-editor-core','loom-workspace-tests','loom-markdown-tests']) assert.ok(runs(ci.jobs.core).includes('cargo test --locked -p ' + crate));
});
test('additional validation is a scope-gated reusable workflow and tools run once per selected platform', () => {
  assert.equal(ci.jobs.validation.uses,'./.github/workflows/validation.yml');
  assert.equal(ci.jobs.validation.if,"needs.changes.outputs.validation == 'true'");
  for (const flag of flags) assert.equal(ci.jobs.validation.with[flag],expr('fromJSON(needs.changes.outputs.' + flag + ')'));
  assert.match(deep.jobs.matrix.if,/inputs.tooling/);
  assert.equal(deep.jobs.matrix.steps.filter(step => (step.run || '').includes('test-tools.mjs')).length,1);
  assert.match(deep.jobs.matrix.steps.find(step => (step.run || '').includes('test-tools.mjs')).if,/inputs.tooling/);
  const validator = deep.jobs.matrix.steps.find(step => (step.run || '').includes('--manifest-path tools/ext-validate/Cargo.toml'));
  assert.equal(validator.run,'cargo test --locked --manifest-path tools/ext-validate/Cargo.toml --test cli');
  assert.equal(validator.if,'inputs.full || inputs.validator');
  assert.match(deep.jobs.matrix.steps.find(step => step.uses === './.github/actions/dependencies').if,/inputs.validator/);
  assert.doesNotMatch(JSON.stringify(deep),/run-wasm\.mjs|inputs\.wasm|scripts\/language-packs/);
  assert.match(runs(deep.jobs.matrix),/production_markdown_evidence/);
  assert.match(runs(deep.jobs.standalone),/standalone-host\.mjs/);
  assert.equal(deep.jobs.standalone.steps.find(step => step.uses === 'dtolnay/rust-toolchain@stable').with.components,'rustfmt');
});
test('dependency setup caches verified archives and Cargo sources without running test suites', () => {
  const deps = load('.github/actions/dependencies/action.yml');
  const source = JSON.stringify(deps);
  assert.doesNotMatch(source,/node --test|smoke\.mjs|npm ci/);
  assert.ok(source.includes('.packaging/downloads'));
  assert.ok(deps.runs.steps.some(step => step.uses === './.github/actions/cargo-cache'));
  const ensure = deps.runs.steps.find(step => (step.run || '').includes('ensure.mjs'));
  assert.equal(ensure.if,"inputs.prepare == 'true'");
  const cache = load('.github/actions/cargo-cache/action.yml');
  const paths = cache.runs.steps[0].with.path;
  assert.match(paths,/~\/\.cargo\/registry/);assert.match(paths,/~\/\.cargo\/git/);
  assert.doesNotMatch(paths,/credentials|~\/\.cargo$/);
});
test('CI result always aggregates all mandatory jobs and the selected reusable validation', () => {
  assert.equal(ci.jobs.result.name,'CI result');assert.equal(ci.jobs.result.if,expr('always()'));
  assert.deepEqual([...ci.jobs.result.needs].sort(),Object.keys(ci.jobs).filter(id => id !== 'result').sort());
  assert.equal(ci.jobs.result.steps.at(-1).env.CI_NEEDS,expr('toJSON(needs)'));
});
test('a single tag entry verifies CI before any platform build and pins all work to that exact SHA', () => {
  assert.deepEqual(release.on.push.tags,['v*.*.*']);
  for (const file of ['release-draft.yml','release-windows.yml','release-linux.yml','release-macos.yml']) assert.equal(fs.existsSync(new URL('../../.github/workflows/' + file,import.meta.url)),false,file);
  const names = [ci.jobs.result.name,ci.jobs.static.name,ci.jobs.core.name,
    ...ci.jobs.build.strategy.matrix.include.map(row => ci.jobs.build.name.replace(expr('matrix.label'),row.label))];
  assert.deepEqual([...requiredChecks].sort(),names.sort());
  assert.equal(release.jobs.verify.permissions.actions,'read');
  assert.equal(release.jobs.verify.outputs.sha,expr('steps.gate.outputs.sha'));
  assert.equal(release.jobs.package.needs,'verify');
  assert.equal(release.jobs.package.steps[0].with.ref,expr('needs.verify.outputs.sha'));
  assert.equal(release.jobs.publish.steps[0].with.ref,expr('needs.verify.outputs.sha'));
  assert.equal(release.concurrency['cancel-in-progress'],false);
});
test('release builds once per platform without repeated CI tests or redundant eager preparation', () => {
  const pack = release.jobs.package, text = JSON.stringify(pack);
  assert.deepEqual(pack.strategy.matrix.include.map(row => row.platform).sort(),['linux','macos','windows']);
  assert.doesNotMatch(text,/cargo test|run-wasm|node --test|launch-smoke\.mjs|production_markdown_evidence/);
  assert.equal(pack.steps.filter(step => step.run === 'node scripts/release/package.mjs').length,1);
  const restore = pack.steps.find(step => step.uses === './.github/actions/dependencies');
  assert.equal(restore.with.prepare,'false');assert.equal(restore.with.variant,'release');
  assert.match(runs(pack),/startup-smoke\.mjs --executable/);
  assert.match(runs(pack),/dpkg-deb --info/);assert.match(runs(pack),/hdiutil verify/);
});
test('only the final job can create/upload a draft, after every platform artifact is verified', () => {
  const publish = release.jobs.publish;
  assert.deepEqual(publish.needs,['verify','package']);
  assert.equal(publish.permissions.contents,'write');
  assert.equal(release.permissions.contents,'read');
  assert.match(runs(publish),/--publish-draft/);
  assert.equal(publish.steps.find(step => step.uses === 'actions/download-artifact@v7').with['merge-multiple'],true);
  assert.doesNotMatch(JSON.stringify(release.jobs.package),/contents.*write|publish\.mjs|release upload/);
  assert.equal(release.on.workflow_dispatch.inputs.publish.default,false);
  assert.equal(release.on.workflow_dispatch.inputs.version.default,undefined);
  assert.equal(deep.env.LOOM_VERSION,undefined);
  assert.ok(deep.jobs.matrix.steps.some(step => step.run === 'node scripts/release/version.mjs --read-env'));
  assert.equal(release.jobs.package.steps.find(step => step.name === 'Build and package once').env.RELEASE_SHA,expr('needs.verify.outputs.sha'));
});
