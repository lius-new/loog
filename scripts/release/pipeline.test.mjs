import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {execFileSync} from 'node:child_process';
import {github} from './github.mjs';
import {requiredChecks, verifyCi} from './verify-ci.mjs';
import {packagePlan, packageRelease} from './package.mjs';
import {platforms, validators, verifyAssets} from './assets.mjs';
import {publishDraft, createDraft, uploadToDraft} from './publish.mjs';
const sha = 'a'.repeat(40), repo = 'test/loom';
const ok = stdout => ({status:0,stdout,stderr:''});
const failed = stderr => ({status:1,stdout:'',stderr});
const runRecord = (extra = {}) => ({id:10,run_number:1,head_sha:sha,head_repository:{full_name:repo},event:'push',status:'completed',conclusion:'success',html_url:'https://github.com/test/loom/actions/runs/10',...extra});
const jobs = () => requiredChecks.map(name => ({name,head_sha:sha,status:'completed',conclusion:'success'}));
function api(runs,checks = jobs()) {
  const calls = [];
  const run = async args => {calls.push(args); return ok(JSON.stringify(args[3].includes('/jobs?') ? [{jobs:checks}] : [{workflow_runs:runs}]));};
  return {run,calls};
}
test('release requires successful exact-SHA CI and the complete native matrix with only two API reads', async () => {
  const f = api([runRecord()]);
  assert.equal((await verifyCi({repo,sha},f.run)).id,10);
  assert.equal(f.calls.length,2);
  assert.ok(f.calls[0][3].includes('head_sha=' + sha));
  assert.ok(f.calls[1][3].includes('filter=latest'));
  assert.ok(f.calls.every(args => args.includes('--paginate') && args.includes('--slurp')));
});
test('PR merge runs, foreign repositories and other commits never satisfy release verification', async () => {
  for (const record of [runRecord({event:'pull_request'}),runRecord({head_sha:'b'.repeat(40)}),runRecord({head_repository:{full_name:'foreign/loom'}})]) {
    const f = api([record]);
    await assert.rejects(verifyCi({repo,sha},f.run),/exact commit/);
    assert.equal(f.calls.length,1);
  }
});
test('latest failed, cancelled or pending CI cannot fall back to an older success', async () => {
  for (const change of [{conclusion:'failure'},{conclusion:'cancelled'},{status:'in_progress',conclusion:null}]) {
    const f = api([runRecord(),runRecord({id:11,run_number:2,...change})]);
    await assert.rejects(verifyCi({repo,sha},f.run),/Finish or rerun CI/);
    assert.equal(f.calls.length,1);
  }
});
test('missing or skipped native checks and checks from another SHA block release', async () => {
  for (const checks of [jobs().slice(1),jobs().map(job => ({...job,conclusion:job.name === 'Test Windows x64' ? 'skipped' : 'success'})),jobs().map(job => ({...job,head_sha:'b'.repeat(40)}))]) {
    await assert.rejects(verifyCi({repo,sha},api([runRecord()],checks).run),/verification missing/);
  }
  await assert.rejects(verifyCi({repo,sha:'--bad'},() => assert.fail('invalid revision reached API')),/Invalid/);
  await assert.rejects(verifyCi({repo,sha},async () => failed('HTTP 403')),/Cannot verify CI/);
});
test('CI pagination includes the successful run and all checks on later pages', async () => {
  const result = await verifyCi({repo,sha},async args => ok(JSON.stringify(args[3].includes('/jobs?')
    ? [{jobs:jobs().slice(0,2)},{jobs:jobs().slice(2)}]
    : [{workflow_runs:[]},{workflow_runs:[runRecord()]}])));
  assert.equal(result.id,10);
});
test('transient GitHub errors retry only the current command and permanent errors fail immediately', async () => {
  const pauses = [], args = ['release','upload','v0.1.3','a.zip','--clobber'];let calls = 0;
  const result = await github(args,{wait:async ms => pauses.push(ms),runner:(_program,actual) => {
    assert.deepEqual(actual,args); return ++calls === 1 ? failed('HTTP 503 Service Unavailable') : ok('uploaded');
  }});
  assert.equal(result.status,0);assert.equal(calls,2);assert.deepEqual(pauses,[1000]);
  for (const reason of ['HTTP 403 permission denied','HTTP 404 not found','HTTP 401 Bad credentials']) {
    calls = 0;
    await github(['api','endpoint'],{wait:async () => assert.fail('permanent error retried'),runner:() => {calls++;return failed(reason);}});
    assert.equal(calls,1);
  }
});
test('transport retries stop at the attempt limit and do not restart builds', async () => {
  let calls = 0;const waits = [];
  const result = await github(['api','endpoint'],{wait:async ms => waits.push(ms),runner:() => {calls++;return {status:null,error:Object.assign(Error('timed out'),{code:'ETIMEDOUT'})};}}).catch(error => error);
  assert.equal(result.code,'ETIMEDOUT');assert.equal(calls,3);assert.deepEqual(waits,[1000,2000]);
});
test('packaging invokes setup, compile, stage and bundle exactly once; failures stop the plan', () => {
  for (const platform of ['windows','linux','macos']) {
    const calls = [];
    packageRelease({platform,version:'0.1.3',sha},{stamp:() => calls.push(['stamp','stamp']),run:(_command,args) => calls.push(args)});
    assert.deepEqual(calls.map(args => args[1]),['stamp','setup','compile','package','bundle']);
    assert.ok(calls.slice(1).every(args => args.includes(platform) && args.includes('0.1.3')));
  }
  const calls = [];
  assert.throws(() => packageRelease({platform:'linux',version:'0.1.3',sha},{stamp:() => {},run:(_command,args) => {calls.push(args[1]);if (args[1] === 'compile') throw Error('build failed');}}),/build failed/);
  assert.deepEqual(calls,['setup','compile']);
  assert.throws(() => packagePlan('other','0.1.3'),/Unsupported/);
  assert.throws(() => packagePlan('linux','../bad'),/Invalid release version/);
});

const hash = bytes => createHash('sha256').update(bytes).digest('hex');
function assetsFixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(),'loom-complete-release-'));
  t.after(() => fs.rmSync(root,{recursive:true,force:true}));
  const assets = path.join(root,'dist');fs.mkdirSync(assets);
  const version = '0.1.3', names = [];
  for (const [target,formats] of Object.entries(platforms)) {
    const prefix = 'Loom-' + version + '-' + target, records = {}, checksums = [];
    for (const [kind,suffix] of Object.entries(formats)) {
      const name = prefix + suffix, bytes = Buffer.from('verified ' + name);
      fs.writeFileSync(path.join(assets,name),bytes);names.push(name);
      records[kind] = {name,sha256:hash(bytes),size:bytes.length};checksums.push(hash(bytes) + '  ' + name);
    }
    fs.writeFileSync(path.join(assets,prefix + '-SHA256SUMS.txt'),checksums.join('\n') + '\n');
    fs.writeFileSync(path.join(assets,'update-' + target + '.json'),JSON.stringify({signature:Buffer.alloc(64).toString('base64'),
      payload:Buffer.from(JSON.stringify({schema:1,version,platform:target,assets:records})).toString('base64')}));
  }
  for (const name of validators) fs.writeFileSync(path.join(assets,name),'validator ' + name);
  const git = (...args) => execFileSync('git',args,{cwd:root,encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim();
  git('init','-b','main');git('config','user.name','CI tests');git('config','user.email','ci@example.invalid');
  fs.writeFileSync(path.join(root,'Cargo.toml'),'[package]\nname = "loom"\nversion = "0.1.3"\n');
  git('add','Cargo.toml');git('commit','-m','Initial release\n\nRelease Notes:\n\n- Added an example.');git('tag','v' + version);
  return {root,assets,version,names,repo,notes:path.join(root,'notes.md')};
}
test('all platform assets, checksums and update metadata are checked before publication', t => {
  const f = assetsFixture(t);
  assert.equal(verifyAssets(f.assets,f.version).length,16);
  fs.unlinkSync(path.join(f.assets,validators[1]));
  assert.throws(() => verifyAssets(f.assets,f.version),/Missing release asset: loom-ext-validate-linux/);
  fs.writeFileSync(path.join(f.assets,validators[1]),'validator');
  fs.writeFileSync(path.join(f.assets,f.names[0]),'corrupted');
  assert.throws(() => verifyAssets(f.assets,f.version),/Checksum mismatch/);
});
test('missing platforms and stale update metadata cannot create a partial draft', async t => {
  for (const failure of ['missing','stale']) {
    const f = assetsFixture(t);
    if (failure === 'missing') fs.unlinkSync(path.join(f.assets,f.names.at(-1)));
    else {
      const file = path.join(f.assets,'update-windows-x86_64.json');
      const envelope = JSON.parse(fs.readFileSync(file));const payload = JSON.parse(Buffer.from(envelope.payload,'base64'));
      payload.version = '0.0.1';envelope.payload = Buffer.from(JSON.stringify(payload)).toString('base64');fs.writeFileSync(file,JSON.stringify(envelope));
    }
    await assert.rejects(publishDraft(f,() => assert.fail('partial release reached GitHub')),/Missing release asset|Invalid update manifest/);
  }
});
test('the single publisher creates and verifies a draft before one complete upload', async t => {
  const f = assetsFixture(t), calls = [], replies = [failed('release not found'),ok('created'),ok('true'),ok('uploaded')];
  await publishDraft(f,args => {calls.push(args);return replies.shift();});
  assert.deepEqual(calls.map(args => args[1]),['view','create','view','upload']);
  assert.equal(calls.at(-1).filter(arg => arg.startsWith(f.assets)).length,16);
  assert.ok(calls[1].includes('--draft'));assert.ok(calls.at(-1).includes('--clobber'));
  assert.ok(!calls.some(args => args[1] === 'edit'));
});
test('reruns reuse the existing draft and published releases are never changed', async t => {
  const f = assetsFixture(t), calls = [];
  await publishDraft(f,args => {calls.push(args[1]);return ok(args[1] === 'view' ? 'true' : 'uploaded');});
  assert.deepEqual(calls,['view','upload']);
  await assert.rejects(publishDraft(f,args => {assert.equal(args[1],'view');return ok('false');}),/already published/);
});
test('missing drafts and permission errors fail without cross-workflow polling or extra writes', async () => {
  let calls = 0;
  await assert.rejects(uploadToDraft({version:'0.1.3',repo,assets:['a.zip']},() => {calls++;return failed('release not found');}),/publish-draft/);
  assert.equal(calls,1);
  calls = 0;
  await assert.rejects(createDraft({version:'0.1.3',repo,notes:'notes'},() => {calls++;return failed('HTTP 403 release not found');}),/Could not inspect/);
  assert.equal(calls,1);
});
