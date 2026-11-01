import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {flags, classify, selectPlan, changedFiles} from './plan.mjs';

const selected = (...names) => Object.fromEntries(flags.map(flag => [flag,names.includes(flag)]));
const sha = letter => letter.repeat(40);
const pull = files => selectPlan({eventName:'pull_request',files});
function fixture(t) {
  const cwd = fs.mkdtempSync(path.join(os.tmpdir(),'loom-ci-scope-'));
  t.after(() => fs.rmSync(cwd,{recursive:true,force:true}));
  const git = (...args) => execFileSync('git',args,{cwd,encoding:'utf8',env:{...process.env,GIT_CONFIG_NOSYSTEM:'1',GIT_CONFIG_GLOBAL:path.join(cwd,'unused-global')}}).trim();
  git('init','-b','main'); git('config','user.name','CI test'); git('config','user.email','ci@example.invalid');
  const put = (file,value) => {fs.mkdirSync(path.dirname(path.join(cwd,file)),{recursive:true});fs.writeFileSync(path.join(cwd,file),value);};
  const commit = () => {git('add','--all');git('commit','-m','fixture');return git('rev-parse','HEAD');};
  return {cwd,git,put,commit};
}

test('ordinary editor, terminal and documentation PRs keep expensive checks off', () => {
  for (const files of [[],['docs/usage.md'],['src/features/terminal/model.rs'],['src/ui/editor/gutter.rs'],['src/app/git.rs']]) {
    assert.deepEqual(pull(files),selected(),files.join(','));
  }
});
test('packaging, licenses and staged resources activate packaging without unrelated integrations', () => {
  for (const file of ['scripts/package/windows/installer.iss','scripts/package-git-runtime.ps1','scripts/build/stage-resources.mjs','LICENSE-MIT','README.md']) {
    assert.deepEqual(pull([file]),selected(...(file.startsWith('scripts/') ? ['tooling','package'] : ['package'])),file);
  }
});
test('Markdown and shared typography changes select evidence; language changes select deep language verification', () => {
  for (const file of ['src/app/markdown_render.rs','src/features/markdown.rs','src/ui/markdown/reader.rs','src/ui/fonts.rs','src/ui/theme.rs','src/domain/view.rs','tests/markdown/scene.json']) {
    assert.deepEqual(pull([file]),selected(...(file.startsWith('tests/') ? ['tooling','evidence'] : file.startsWith('src/domain/') ? ['evidence','validator'] : ['evidence'])),file);
  }
  for (const file of ['src/features/syntax/treesitter.rs','src/infrastructure/language_packs.rs','tools/ext-validate/src/lib.rs','tools/ext-validate/tests/cli.rs','src/domain/language/registry.rs']) {
    assert.deepEqual(pull([file]),selected(...(file.startsWith('tools/') ? ['tooling','evidence','validator','standalone'] : ['evidence','validator','standalone'])),file);
  }
  assert.deepEqual(pull(['tests/fixtures/extensions/loom.lang.json/tree-sitter-json.wasm']),selected('tooling','evidence','validator','standalone'));
  assert.deepEqual(pull(['assets/registry/extensions.lock']),selected('tooling','package','evidence'));
  assert.deepEqual(pull(['icons/app.ico']),selected('tooling','package','evidence'));
  assert.deepEqual(pull(['src/main.rs']),selected('standalone'));
  assert.deepEqual(pull(['src/features/extensions/manifest.rs']),selected('validator'));
  assert.deepEqual(pull(['src/infrastructure/extensions/state.rs']),selected('validator'));
  assert.deepEqual(pull(['crates/editor-core/src/text.rs']),selected('evidence','validator'));
});
test('policy, dependency, unknown and invalid paths conservatively select every optional check', () => {
  for (const file of ['Cargo.toml','Cargo.lock','rust-toolchain.toml','build.rs','.github/workflows/ci.yml','.cargo/config.toml',
    'vendor/vt100/src/lib.rs','scripts/ci/plan.mjs','scripts/dependencies/manifest.json','scripts/lib/download.mjs','tools/unknown/config','future-build-system/config', '../escape', 'src\\odd.rs', null]) {
    assert.deepEqual(classify([file]),selected(...flags),String(file));
  }
  assert.deepEqual(pull(null),selected(...flags));
});
test('main pushes stay scoped; only explicit full validation enables every additional task', () => {
  assert.deepEqual(selectPlan({eventName:'push',files:['src/app/git.rs']}),selected());
  assert.deepEqual(selectPlan({eventName:'push',files:['scripts/package/linux/index.mjs']}),selected('tooling'));
  assert.deepEqual(selectPlan({eventName:'workflow_dispatch',event:{inputs:{full:true}}}),selected(...flags));
  assert.deepEqual(selectPlan({eventName:'workflow_dispatch',event:{inputs:{full:'false'}}}),selected());
  assert.deepEqual(selectPlan({eventName:'workflow_dispatch',event:{inputs:{full:false}}}),selected());
  assert.deepEqual(selectPlan({eventName:'workflow_dispatch'}),selected(...flags));
  assert.deepEqual(selectPlan({eventName:'merge_group',files:[]}),selected(...flags));
});
test('diff parsing uses three-dot PR history, two-dot pushes and NUL paths without rename folding', () => {
  for (const eventName of ['pull_request','push']) {
    const calls = [], event = {before:sha('a'),after:sha('b'),pull_request:{base:{sha:sha('a')},head:{sha:sha('b')}}};
    const files = changedFiles({eventName,event,git:(command,args) => {calls.push({command,args});return 'scripts/package/old.mjs\0docs/new.mjs\0docs/line\nbreak.md\0';}});
    assert.deepEqual(files,['scripts/package/old.mjs','docs/new.mjs','docs/line\nbreak.md']);
    assert.equal(calls[0].command,'git');
    assert.ok(calls[0].args.includes('--no-renames'));
    assert.ok(calls[0].args.includes(sha('a') + (eventName === 'pull_request' ? '...' : '..') + sha('b')));
    assert.deepEqual(classify(files),selected('tooling','package'));
  }
});
test('missing history, first pushes and invalid revisions fail open to full checks', () => {
  const warnings = [];
  for (const event of [{before:'0'.repeat(40),after:sha('a')},{before:'--bad',after:sha('a')},{}]) {
    assert.equal(changedFiles({eventName:'push',event,warn:message => warnings.push(message),git:() => assert.fail('unsafe revision must not reach git')}),null);
  }
  assert.equal(changedFiles({eventName:'push',event:{before:sha('a'),after:sha('b')},git:() => {throw Error('shallow history');},warn:message => warnings.push(message)}),null);
  assert.equal(warnings.length,4);
  assert.deepEqual(selectPlan({eventName:'push',files:null}),selected(...flags.filter(flag => flag !== 'package')));
});
test('real git PR diffs ignore base-only commits and preserve deleted packaging scope after a rename', t => {
  const f = fixture(t);
  f.put('scripts/package/fixture.txt','package'); f.commit();
  f.git('checkout','-b','feature'); f.put('docs/usage.md','docs'); const head = f.commit();
  f.git('checkout','main'); f.put('Cargo.lock','base-only'); const base = f.commit();
  const event = {pull_request:{base:{sha:base},head:{sha:head}}};
  assert.deepEqual(changedFiles({eventName:'pull_request',event,cwd:f.cwd}),['docs/usage.md']);
  f.git('checkout','feature'); f.git('mv','scripts/package/fixture.txt','docs/moved.txt');
  const renamed = f.commit(); event.pull_request.head.sha = renamed;
  const files = changedFiles({eventName:'pull_request',event,cwd:f.cwd});
  assert.ok(files.includes('scripts/package/fixture.txt')); assert.ok(files.includes('docs/moved.txt'));
  assert.deepEqual(pull(files),selected('tooling','package'));
});
test('planner CLI produces literal boolean outputs and a summary for the event actually checked out', t => {
  const f = fixture(t); f.put('docs/a.md','a'); const before = f.commit();
  f.put('docs/a.md','b'); const after = f.commit();
  const eventPath = path.join(f.cwd,'event.json'), output = path.join(f.cwd,'output'), summary = path.join(f.cwd,'summary');
  fs.writeFileSync(eventPath,JSON.stringify({pull_request:{base:{sha:before},head:{sha:after}}}));
  execFileSync(process.execPath,[fileURLToPath(new URL('./plan.mjs',import.meta.url))],{cwd:f.cwd,
    env:{...process.env,GITHUB_EVENT_NAME:'pull_request',GITHUB_EVENT_PATH:eventPath,GITHUB_OUTPUT:output,GITHUB_STEP_SUMMARY:summary}});
  assert.equal(fs.readFileSync(output,'utf8'),'tooling=false\nevidence=false\nvalidator=false\nstandalone=false\npackage=false\nvalidation=false\n');
  assert.match(fs.readFileSync(summary,'utf8'),/Three-platform host tests/);
});
