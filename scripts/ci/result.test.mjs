import test from 'node:test';
import assert from 'node:assert/strict';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {checkResults} from './result.mjs';
import {flags} from './plan.mjs';
function successful(selected = []) {
  const needs = Object.fromEntries(['changes','static','core','build','validation'].map(job => [job,{result:'success'}]));
  needs.changes.outputs = Object.fromEntries(flags.map(flag => [flag,String(selected.includes(flag))]));
  needs.changes.outputs.validation = String(selected.length > 0);
  if (!selected.length) needs.validation.result = 'skipped';
  return needs;
}
test('ordinary CI accepts a legitimate validation skip; each selected group requires success', () => {
  assert.deepEqual(checkResults(successful()),[]);
  for (const flag of flags) {
    assert.deepEqual(checkResults(successful([flag])),[]);
    for (const state of ['failure','cancelled','skipped',undefined]) {
      const needs = successful([flag]); needs.validation.result = state;
      assert.ok(checkResults(needs).some(error => error.startsWith('validation:')));
    }
  }
});
test('mandatory jobs cannot fail, cancel, disappear or skip', () => {
  for (const job of ['changes','static','core','build']) for (const state of ['failure','cancelled','skipped',undefined]) {
    const needs = successful();needs[job].result = state;
    assert.ok(checkResults(needs).some(error => error.startsWith(job + ':')));
  }
});
test('invalid outputs or an inconsistent validation flag cannot disable selected checks', () => {
  for (const flag of [...flags,'validation']) for (const value of ['',undefined,true,false]) {
    const needs = successful();needs.changes.outputs[flag] = value;
    assert.ok(checkResults(needs).some(error => error.includes('invalid selection output: ' + flag)));
  }
  const needs = successful(['validator']);needs.changes.outputs.validation = 'false';
  assert.ok(checkResults(needs).includes('inconsistent validation selection'));
});
test('aggregate CLI preserves failures without rebuilding or invoking remote services', () => {
  const file = fileURLToPath(new URL('./result.mjs',import.meta.url));
  const pass = spawnSync(process.execPath,[file],{env:{...process.env,CI_NEEDS:JSON.stringify(successful())},encoding:'utf8'});
  assert.equal(pass.status,0,pass.stderr);
  const needs = successful();needs.build.result = 'failure';
  const fail = spawnSync(process.execPath,[file],{env:{...process.env,CI_NEEDS:JSON.stringify(needs)},encoding:'utf8'});
  assert.equal(fail.status,1);assert.match(fail.stderr,/build: failure/);
});
