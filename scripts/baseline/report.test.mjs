import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {spawnSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {BENCHMARKS} from './cases.mjs';
import {evaluateReport, parseMeasurements, validateExactTest} from './report-core.mjs';

const spec = BENCHMARKS.find(item => item.id === 'tab-pointer');
const summary = (passed = 1) => `test result: ok. ${passed} passed; 0 failed; 0 ignored; 0 measured; 200 filtered out`;
const text = value => `Running unittests src/main.rs (target/release/deps/Loog-deadbeef.exe)\ntest ${spec.filter} ... 100 tabs: ${value}ms per pointer update\n500 tabs: ${value * 2}ms per pointer update\nok\n${summary()}`;
function fixture() {
  const logs = Object.fromEntries([0, 1, 2].map(sample => [`perf-tab-pointer-${sample}.log`, text(sample === 0 ? 1000 : sample)]));
  const checks = Object.keys(logs).map(log => ({log: `logs/${log}`, exit_code: 0, exact_filter: spec.filter}));
  return {logs, checks, environment: {benchmarks: ['tab-pointer'], perf_samples: 2}};
}

test('exact checks require the named test, including serial nocapture output, and reject zero matches', () => {
  assert.equal(validateExactTest(text(1), spec.filter), true);
  assert.equal(validateExactTest(summary(0), spec.filter), false);
  assert.equal(validateExactTest(text(1).replaceAll(spec.filter, 'unrelated::test'), spec.filter), false);
});

test('warmup is excluded, median uses formal samples, and each suite retains its crate', () => {
  const report = evaluateReport(fixture());
  assert.equal(report.complete, true, JSON.stringify(report.notices));
  assert.equal(report.records.length, 4);
  assert.equal(report.statistics[0].median, 1.5);
  assert.equal(report.statistics[0].samples, 2);
  assert.equal(report.suites[0].crate, 'loog');
});

test('missing samples, missing metrics and duplicate records are explicit failures', () => {
  const missing = fixture();
  delete missing.logs['perf-tab-pointer-2.log'];
  missing.checks.pop();
  assert.ok(evaluateReport(missing).notices.some(item => item.reason.includes('Expected sample is missing')));
  const metric = fixture();
  metric.logs['perf-tab-pointer-1.log'] = metric.logs['perf-tab-pointer-1.log'].replace(/500 tabs:.+\n/, '');
  assert.equal(evaluateReport(metric).complete, false);
  assert.ok(evaluateReport(metric).notices.some(item => item.reason.includes('Missing metric: 500-tabs/pointer_update')));
  const duplicate = fixture();
  duplicate.logs['perf-tab-pointer-1.log'] += '\n100 tabs: 2ms per pointer update';
  assert.ok(evaluateReport(duplicate).notices.some(item => item.reason.includes('Duplicate')));
});

test('nonzero commands override earlier passing summaries; missing logs and null exit codes fail', () => {
  const input = fixture();
  input.logs['clippy.log'] = 'error: cannot compile';
  input.checks.push({log: 'logs/clippy.log', exit_code: 101}, {log: 'logs/missing.log', exit_code: null});
  input.checks[1].exit_code = 101;
  const report = evaluateReport(input);
  assert.equal(report.complete, false);
  assert.equal(report.checks.find(item => item.log === 'clippy.log').result, 'FAILED');
  assert.ok(report.notices.some(item => item.reason === 'Expected log is missing'));
  assert.equal(report.records.length, 2); // Failed timing output is not sampled.
});

test('unobserved search cancellation is a warning with no fabricated measurement', () => {
  const search = BENCHMARKS.find(item => item.id === 'search');
  const output = `test ${search.filter} ... LOOG_BASELINE {"scenario":"100-files-6.25m","metric":"search_scan_cached","value":2,"unit":"ms"}\nLOOG_BASELINE_STATUS cancellation_not_observed scan_completed_before_request\nok\n${summary()}`;
  const report = evaluateReport({logs: {'perf-search-1.log': output}, checks: [{log: 'logs/perf-search-1.log', exit_code: 0}]});
  assert.equal(report.complete, true);
  assert.ok(report.notices.some(item => item.reason.includes('Not measured:') && item.severity === 'warning'));
  assert.equal(report.records.length, 1);
});

test('old JSON markers and human timing formats remain readable and malformed values fail', () => {
  const parsed = parseMeasurements([
    '1 MB: typing 2µs/char, paste 1 MB 1ms, undo 1ms, select-all indent 2ms, history 3.0 MB',
    '8 MiB single line: 8.0 MiB, prepare 1ms, 100 viewport renders and clicks 2ms',
    '8 MiB single line: 20 end-of-file edits and renders 3ms',
    'LOOG_BASELINE {"scenario":"file","metric":"read","value":2,"unit":"ms"}',
    'R7_TERMINAL_PERF {"sessions":8,"metric":"frame_projection_500","milliseconds":3,"iterations":500,"nodes":0}',
  ].join('\n'), 1);
  assert.equal(parsed.rows.length, 10);
  assert.equal(parsed.rows[0].value, 0.002);
  assert.ok(parsed.rows.some(row => row.scenario === 'terminal-8-sessions'));
  assert.throws(() => parseMeasurements('LOOG_BASELINE {"scenario":"x","metric":"y","value":-1,"unit":"ms"}', 1), /Invalid measurement/);
});

test('historical logs are marked unverified and missing warmup is detected from old environment metadata', () => {
  const report = evaluateReport({logs: {'perf-tab-pointer-1.log': text(1)}, environment: {performance_set: 'Legacy', perf_samples: 1}});
  assert.equal(report.verified, false);
  assert.equal(report.complete, false);
  assert.ok(report.notices.some(item => item.log === 'perf-tab-pointer-0.log'));
});

test('historical module paths remain readable without weakening new exact-filter checks', () => {
  const input = fixture();
  input.logs = Object.fromEntries(Object.entries(input.logs).map(([name, output]) => [name,
    output.replaceAll(spec.filter, 'ui::tabs::tests::pointer_updates_with_many_tabs')]));
  for (const check of input.checks) delete check.exact_filter;
  assert.equal(evaluateReport(input).complete, true);
  input.environment.schema = 2;
  assert.equal(evaluateReport(input).complete, false);
});

test('CLI writes failure artifacts and returns nonzero rather than hiding a compile failure', t => {
  const directory = fs.mkdtempSync(path.join(os.tmpdir(), 'loog-baseline-report-'));
  t.after(() => fs.rmSync(directory, {recursive: true, force: true}));
  fs.mkdirSync(path.join(directory, 'logs'));
  fs.writeFileSync(path.join(directory, 'logs/build.log'), 'error: could not compile');
  fs.writeFileSync(path.join(directory, 'checks.json'), JSON.stringify([{log: 'logs/build.log', exit_code: 101}]));
  const result = spawnSync(process.execPath, [fileURLToPath(new URL('./report.mjs', import.meta.url)), directory], {encoding: 'utf8'});
  assert.equal(result.status, 1, result.stderr);
  assert.match(fs.readFileSync(path.join(directory, 'measurements.md'), 'utf8'), /build.log.*FAILED.*101/);
  assert.equal(JSON.parse(fs.readFileSync(path.join(directory, 'measurements.json'))).complete, false);
});

test('the language-service benchmark remains an explicit release measurement', () => {
  const spec = BENCHMARKS.find(item => item.id === 'language-service');
  const sample = parseMeasurements('LOOG_BASELINE {"scenario":"8 MiB document","metric":"incremental_edit","value":12.345,"unit":"ms"}', 1).rows;
  assert.ok(spec.filter.endsWith('planning_an_edit_in_a_big_document_is_fast'));
  assert.equal(sample.length, 1);
  assert.equal(sample[0].scenario + '/' + sample[0].metric, spec.metrics[0]);
});
