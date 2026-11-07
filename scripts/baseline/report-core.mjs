import {BENCHMARKS, benchmarkForLog, selectBenchmarks} from './cases.mjs';
import {parseTestResults, parseTestSuites} from './test-results.mjs';

const duration = value => {
  const match = /^([\d.]+)(ns|µs|us|ms|s)$/.exec(value);
  if (!match) throw Error(`Unknown duration: ${value}`);
  return Number(match[1]) * ({ns: 0.000001, 'µs': 0.001, us: 0.001, ms: 1, s: 1000})[match[2]];
};
export function parseMeasurements(text, sample) {
  const rows = [], notices = [];
  const add = (scenario, metric, value, unit = 'ms', extra = {}) => rows.push({scenario, metric, value, unit, sample, ...extra});
  for (const line of text.split(/\r?\n/)) {
    const marker = line.indexOf('LOOG_BASELINE ');
    if (marker >= 0) rows.push({...JSON.parse(line.slice(marker + 14)), sample});
    const terminal = line.indexOf('R7_TERMINAL_PERF ');
    if (terminal >= 0) {
      const record = JSON.parse(line.slice(terminal + 17));
      add(`terminal-${record.sessions}-sessions`, record.metric, record.milliseconds, 'ms', {
        sessions: record.sessions, iterations: record.iterations, nodes: record.nodes,
        viewport: record.viewport, scale: record.scale, screen: record.screen});
    }
    if (line.includes('LOOG_BASELINE_STATUS')) notices.push(line.trim());
    let match = /(\d+) MB: typing (\S+)\/char, paste 1 MB (\S+), undo (\S+), select-all indent (\S+), history ([\d.]+) MB/.exec(line);
    if (match) {
      ['typing_per_char', 'paste_1m', 'undo_paste', 'select_all_indent'].forEach((metric, i) => add(`ascii-${match[1]}m`, metric, duration(match[i + 2])));
      add(`ascii-${match[1]}m`, 'history_bytes_rounded', Number(match[6]) * 1024 * 1024, 'bytes');
    }
    match = /(8 MiB single line|Unicode single line|many short lines): ([\d.]+) MiB, prepare (\S+), 100 viewport renders and clicks (\S+)/.exec(line);
    if (match) {
      add(match[1], 'ui_document_prepare', duration(match[3]));
      add(match[1], 'ui_100_viewports_and_clicks', duration(match[4]));
    }
    match = /(8 MiB single line|Unicode single line|many short lines): 20 end-of-file edits and renders (\S+)/.exec(line);
    if (match) add(match[1], 'ui_20_edits_and_renders', duration(match[2]));
    match = /(100|500) tabs: (\S+) per pointer update/.exec(line);
    if (match) add(`${match[1]}-tabs`, 'pointer_update', duration(match[2]));
  }
  if (rows.some(row => typeof row.scenario !== 'string' || typeof row.metric !== 'string' ||
    !Number.isFinite(row.value) || row.value < 0 || typeof row.unit !== 'string')) throw Error('Invalid measurement record');
  return {rows, notices};
}

export function validateExactTest(text, filter) {
  const result = parseTestResults(text);
  const tests = parseTestSuites(text).flatMap(suite => suite.tests);
  return result?.result === 'ok' && result.passed === 1 && result.failed === 0 &&
    tests.filter(test => test.name === filter && test.result === 'ok').length === 1;
}

// Pure evaluation, shared by the CLI and fixture tests. Exit status takes
// precedence over a successful suite printed before a later build/doc failure.
export function evaluateReport({logs, checks: recorded, environment = {}}) {
  const rows = [], checks = [], notices = [], suites = [], inventory = [], diagnostics = new Map();
  let complete = true;
  const notice = (log, reason, failure = true) => {
    notices.push({log, reason, severity: failure ? 'error' : 'warning'});
    if (failure) complete = false;
  };
  if (!recorded) notice('checks.json', 'No recorded exit codes; historical logs are unverified', false);
  if (!environment.perf_samples && Object.keys(logs).some(name => benchmarkForLog(name))) notice('environment.json', 'Expected sample count is unavailable', false);
  const metadata = new Map((recorded || []).map(check => [check.log.replaceAll('\\', '/').replace(/^logs\//, ''), check]));
  const names = new Set([...Object.keys(logs), ...metadata.keys()]);
  for (const name of [...names].sort()) {
    const entry = metadata.get(name), text = logs[name], benchmark = benchmarkForLog(name);
    if (recorded && !entry) notice(name, 'Log has no recorded exit code');
    if (text === undefined) {
      checks.push({...entry, log: name, result: 'FAILED', suites: []});
      notice(name, 'Expected log is missing');
      continue;
    }
    const parsedSuites = parseTestSuites(text), summary = parseTestResults(text);
    const result = entry ? (entry.exit_code === 0 && summary?.result !== 'FAILED' ? 'ok' : 'FAILED') : (summary?.result || 'unverified');
    const check = {...entry, ...summary, log: name, result, suites: parsedSuites};
    checks.push(check);
    suites.push(...parsedSuites.map(suite => ({...suite, log: name})));
    inventory.push(...parsedSuites.flatMap(suite => suite.tests.map(test => ({...test, crate: suite.crate, kind: suite.kind, target: suite.target, log: name}))));
    if (result === 'FAILED') notice(name, `Check failed (exit ${entry?.exit_code ?? 'unknown'})`);
    if (entry?.exact_filter && !validateExactTest(text, entry.exact_filter)) notice(name, `Exact test did not pass once: ${entry.exact_filter}`);
    for (const match of text.matchAll(/error: ([^\r\n]+)\r?\n\s+--> ((?:src|crates)[\\/][^:\r\n]+):(\d+):(\d+)/g)) {
      const file = match[2].replaceAll('\\', '/');
      diagnostics.set(`${file}:${match[3]}`, {message: match[1], file, line: Number(match[3]), column: Number(match[4])});
    }
    if (!benchmark) continue;
    const {benchmark: spec, sample} = benchmark;
    // Older runs predate exact_filter metadata and may use a retired module
    // path. Keep the function identity, passed count and exit-code checks.
    const historicalName = parsedSuites.flatMap(suite => suite.tests)
      .find(test => test.name.split('::').at(-1) === spec.filter.split('::').at(-1))?.name;
    const filter = entry?.exact_filter || (environment.schema === 2 ? spec.filter : historicalName) || spec.filter;
    if (!validateExactTest(text, filter) || (entry && entry.exit_code !== 0)) {
      notice(name, 'Invalid performance sample: exact test failed or was not executed');
      continue;
    }
    let measurements;
    try { measurements = parseMeasurements(text, sample); }
    catch (error) { notice(name, error.message); continue; }
    const present = new Set(measurements.rows.map(row => `${row.scenario}/${row.metric}`));
    for (const metric of spec.metrics) if (!present.has(metric)) notice(name, `Missing metric: ${metric}`);
    for (const metric of spec.optionalMetrics || []) if (!present.has(metric)) notice(name, `Not measured: ${metric} (cancellation may have completed before request)`, false);
    for (const reason of measurements.notices) notice(name, reason, false);
    if (present.size !== measurements.rows.length) notice(name, 'Duplicate measurements in one sample');
    if (sample > 0) rows.push(...measurements.rows.map(row => ({...row, log: name})));
  }
  let expected = [];
  if (Array.isArray(environment.benchmarks)) {
    expected = environment.benchmarks.map(id => {
      const spec = BENCHMARKS.find(item => item.id === id);
      if (!spec) notice('environment.json', `Unknown benchmark: ${id}`);
      return spec;
    }).filter(Boolean);
  } else if (environment.performance_set) {
    const selection = {All: 'all', Legacy: 'editor,ui', Additional: 'file,search,terminal', None: 'none'}[environment.performance_set] ?? environment.performance_set;
    expected = selectBenchmarks(selection);
  }
  for (const spec of expected) {
    for (let sample = 0; sample <= environment.perf_samples; sample++) {
      const matching = [...names].find(name => {
        const item = benchmarkForLog(name);
        return item?.benchmark.id === spec.id && item.sample === sample;
      });
      if (!matching) notice(`perf-${spec.id}-${sample}.log`, 'Expected sample is missing');
    }
  }
  const groups = new Map();
  for (const row of rows) {
    const key = `${row.scenario}/${row.metric}/${row.unit}`;
    if (!groups.has(key)) groups.set(key, []);
    groups.get(key).push(row);
  }
  const statistics = [...groups.values()].map(records => {
    const values = records.map(row => row.value).sort((a, b) => a - b), middle = Math.floor(values.length / 2);
    return {scenario: records[0].scenario, metric: records[0].metric, unit: records[0].unit, samples: values.length,
      median: values.length % 2 ? values[middle] : (values[middle - 1] + values[middle]) / 2, min: values[0], max: values.at(-1)};
  });
  return {schema: 2, complete, verified: Boolean(recorded), records: rows, statistics, checks, suites, notices,
    inventory, diagnostics: [...diagnostics.values()]};
}
