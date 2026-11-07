import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {optionsFromArgs, buildPlan, executeCheck, runBaseline} from './run.mjs';

const temporary = t => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'loog-baseline-run-'));
  t.after(() => fs.rmSync(directory, {recursive: true, force: true, maxRetries: 5}));
  return directory;
};
test('named performance groups and independent check modes replace historical phases', () => {
  const options = optionsFromArgs(['--performance', 'ui', '--samples', '2']);
  const plan = buildPlan(options);
  assert.equal(plan.length, 6);
  assert.ok(plan.some(check => check.exact_filter === 'app::tabs::tests::pointer_updates_with_many_tabs'));
  assert.ok(plan.every(check => check.args.includes('--exact') && check.args.includes('--release') && check.args.includes('Loog')));
  const checks = buildPlan(optionsFromArgs(['--performance', 'none', '--checks']));
  assert.ok(checks.some(check => check.name === 'tests-release'));
  assert.ok(!checks.some(check => check.name === 'tests-isolated' || check.name.startsWith('native-')));
  assert.throws(() => optionsFromArgs(['--performance', 'Legacy']), /Unknown performance group/);
  assert.throws(() => optionsFromArgs(['--samples', '0']), /between 1 and 20/);
  assert.throws(() => optionsFromArgs(['--performance', 'none']), /No work/);
});

test('child execution streams both outputs and preserves failure status without modifying parent environment', async t => {
  const directory = temporary(t);
  fs.mkdirSync(path.join(directory, 'logs'));
  const previous = process.env.LOOG_BASELINE_CHILD;
  const result = await executeCheck({program: process.execPath, args: ['-e',
    'console.log(process.env.LOOG_BASELINE_CHILD); console.error("stderr evidence"); process.exitCode = 7;'], log: 'logs/check.log'},
  {directory, env: {...process.env, LOOG_BASELINE_CHILD: 'isolated'}});
  assert.equal(result.exit_code, 7);
  assert.equal(process.env.LOOG_BASELINE_CHILD, previous);
  assert.match(fs.readFileSync(path.join(directory, 'logs/check.log'), 'utf8'), /isolated[\s\S]*stderr evidence/);
});

test('child spawn failure is recorded as a failed check', async t => {
  const directory = temporary(t);
  const result = await executeCheck({program: path.join(directory, 'missing-program'), args: [], log: 'missing.log'}, {directory, env: process.env});
  assert.equal(result.exit_code, 1);
  assert.match(result.error, /ENOENT/);
});

test('cancellation ends the owned child and records a nonzero result', async t => {
  const directory = temporary(t), controller = new AbortController();
  const pending = executeCheck({program: process.execPath, args: ['-e', 'setInterval(() => {}, 1000)'], log: 'cancelled.log'},
    {directory, env: process.env, signal: controller.signal});
  const timer = setTimeout(() => controller.abort(), 100);
  try {assert.notEqual((await pending).exit_code, 0);}
  finally {clearTimeout(timer);}
});

test('runner rejects zero-match benchmarks, continues other cases and publishes incomplete artifacts', async t => {
  const root = temporary(t), output = path.join(root, 'run');
  const options = optionsFromArgs(['--output-directory', output, '--performance', 'ui', '--samples', '1', '--offline']);
  const previous = process.env.GIT_CONFIG_GLOBAL;
  const called = [];
  const code = await runBaseline(options, {
    prepare: async ({offline}) => {assert.equal(offline, true); return {env: {...process.env}};},
    execute: async (check, context) => {
      called.push(check.name);
      assert.equal(context.env.GIT_CONFIG_NOSYSTEM, '1');
      assert.equal(context.env.GIT_CONFIG_GLOBAL, path.join(output, 'controlled-git.config'));
      const text = check.benchmark === 'ui-interaction' ? 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 999 filtered out' :
        `test ${check.exact_filter} ... 100 tabs: 1ms per pointer update\n500 tabs: 2ms per pointer update\nok\ntest result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out`;
      return executeCheck({...check, program: process.execPath, args: ['-e', `process.stdout.write(${JSON.stringify(text)})`]}, context);
    },
  });
  assert.equal(code, 1);
  assert.equal(process.env.GIT_CONFIG_GLOBAL, previous);
  assert.ok(called.includes('perf-tab-pointer-1'));
  assert.ok(!called.includes('perf-ui-interaction-1'));
  const checks = JSON.parse(fs.readFileSync(path.join(output, 'checks.json')));
  assert.match(checks.find(check => check.name === 'perf-ui-interaction-0').error, /Exact test/);
  const report = JSON.parse(fs.readFileSync(path.join(output, 'measurements.json')));
  assert.equal(report.complete, false);
  assert.equal(report.statistics.length, 2);
  await assert.rejects(runBaseline(options), /fresh/);
});

test('dependency failure still leaves a report and nonzero result', async t => {
  const options = optionsFromArgs(['--output-directory', path.join(temporary(t), 'failed'), '--performance', 'file', '--samples', '1']);
  assert.equal(await runBaseline(options, {prepare: async () => {throw Error('Offline cache unavailable');}}), 1);
  const report = JSON.parse(fs.readFileSync(path.join(options.output, 'measurements.json')));
  assert.equal(report.checks.find(check => check.name === 'dependency-prepare').exit_code, 1);
  assert.ok(report.notices.some(item => item.reason === 'Expected log is missing'));
});
