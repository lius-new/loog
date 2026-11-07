import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {createHash} from 'node:crypto';
import {spawn, spawnSync} from 'node:child_process';
import {parseArgs} from 'node:util';
import {fileURLToPath} from 'node:url';
import {performance} from 'node:perf_hooks';
import {prepareDependencies, ROOT} from '../dependencies/ensure.mjs';
import {NATIVE_SMOKES, selectBenchmarks} from './cases.mjs';
import {generateReport} from './report.mjs';
import {validateExactTest} from './report-core.mjs';

const HERE = path.dirname(fileURLToPath(import.meta.url));
const json = (file, value) => fs.writeFileSync(file, JSON.stringify(value, null, 2) + '\n');
const hash = data => createHash('sha256').update(data).digest('hex');
const version = (program, args, env) => {
  const result = spawnSync(program, args, {cwd: ROOT, env, encoding: 'utf8', windowsHide: true});
  return result.status === 0 ? result.stdout.trim() : {unavailable: result.error?.message || result.stderr?.trim() || `exit ${result.status}`};
};

export function optionsFromArgs(args) {
  const {values} = parseArgs({args, options: {
    help: {type: 'boolean'}, 'output-directory': {type: 'string'}, samples: {type: 'string', default: '5'},
    performance: {type: 'string', default: 'all'}, checks: {type: 'boolean'}, isolated: {type: 'boolean'},
    'native-smoke': {type: 'boolean'}, offline: {type: 'boolean'}, 'skia-archive': {type: 'string'},
  }});
  if (values.help) return {help: true};
  const samples = Number(values.samples);
  if (!Number.isInteger(samples) || samples < 1 || samples > 20) throw Error('--samples must be an integer between 1 and 20');
  const benchmarks = selectBenchmarks(values.performance);
  if (!benchmarks.length && !values.checks && !values.isolated && !values['native-smoke']) throw Error('No work selected; use --checks, --isolated or --native-smoke with --performance none');
  if (values['native-smoke'] && process.platform !== 'win32') throw Error('--native-smoke requires Windows');
  return {samples, benchmarks, performance: values.performance, checks: Boolean(values.checks), isolated: Boolean(values.isolated),
    nativeSmoke: Boolean(values['native-smoke']), offline: Boolean(values.offline),
    skiaArchive: values['skia-archive'] ? path.resolve(values['skia-archive']) : undefined,
    output: values['output-directory'] ? path.resolve(values['output-directory']) : path.join(ROOT, 'target/baseline', new Date().toISOString().replace(/[:.]/g, '-'))};
}

export function buildPlan(options) {
  const plan = [], locked = ['--locked', ...(options.offline ? ['--offline'] : [])];
  const add = (name, program, args, extra = {}) => plan.push({name, program, args, log: `logs/${name}.log`, exit_code: null, ...extra});
  if (options.checks) {
    const architectureTests = ['check', 'markdown', 'standalone-host', 'language-resources'].map(name => `scripts/architecture/${name}.test.mjs`);
    const baselineTests = fs.readdirSync(HERE).filter(name => name.endsWith('.test.mjs')).sort().map(name => `scripts/baseline/${name}`);
    add('architecture-self-test', process.execPath, ['--test', ...architectureTests]);
    add('architecture-gate', process.execPath, ['scripts/architecture/check.mjs']);
    add('baseline-self-test', process.execPath, ['--test', ...baselineTests]);
    add('format', 'cargo', ['fmt', '--all', '--', '--check']);
    add('clippy', 'cargo', ['clippy', ...locked, '--workspace', '--all-targets', '--', '-D', 'warnings',
      ...['redundant_closure_call', 'collapsible_if', 'too_many_arguments', 'let_and_return', 'manual_div_ceil'].flatMap(name => ['-A', `clippy::${name}`])]);
    add('tests-native', 'cargo', ['test', ...locked, '--workspace', '--', '--nocapture']);
    add('tests-release', 'cargo', ['test', '--release', ...locked, '--workspace', '--', '--nocapture']);
    add('release-app-build', 'cargo', ['build', '--release', ...locked, '--bin', 'Loog']);
  }
  if (options.isolated) add('tests-isolated', 'cargo', ['test', ...locked, '--workspace', '--', '--nocapture'], {isolated: true});
  const exact = filter => ['test', '--release', ...locked, '-p', 'loog', '--bin', 'Loog', filter,
    '--', '--exact', '--ignored', '--nocapture', '--test-threads=1'];
  if (options.nativeSmoke) for (const smoke of NATIVE_SMOKES) add(smoke.id, 'cargo', exact(smoke.filter), {isolated: true, exact_filter: smoke.filter});
  for (const benchmark of options.benchmarks) for (let sample = 0; sample <= options.samples; sample++) {
    add(`perf-${benchmark.id}-${sample}`, 'cargo', exact(benchmark.filter),
      {isolated: true, exact_filter: benchmark.filter, benchmark: benchmark.id, sample});
  }
  return plan;
}

export function executeCheck(check, {directory, env, signal}) {
  return new Promise(resolve => {
    const descriptor = fs.openSync(path.join(directory, check.log), 'w');
    let child;
    try {
      child = spawn(check.program, check.args, {cwd: ROOT, env, stdio: ['ignore', descriptor, descriptor], windowsHide: true});
    } catch (error) {
      fs.writeSync(descriptor, error.stack + '\n');
      fs.closeSync(descriptor);
      resolve({exit_code: 1, error: error.message});
      return;
    }
    let error;
    const abort = () => child.kill(); // Only this invocation's child is eligible.
    signal?.addEventListener('abort', abort, {once: true});
    if (signal?.aborted) abort();
    child.on('error', value => {error = value.message; fs.writeSync(descriptor, value.stack + '\n');});
    child.on('close', (code, termination) => {
      signal?.removeEventListener('abort', abort);
      fs.closeSync(descriptor);
      resolve({exit_code: error ? 1 : (code ?? 1), ...(error ? {error} : {}), ...(termination ? {signal: termination} : {})});
    });
  });
}

export async function runBaseline(options, {prepare = prepareDependencies, execute = executeCheck, signal} = {}) {
  if (fs.existsSync(options.output) && fs.readdirSync(options.output).length) throw Error('Choose a fresh, empty output directory');
  const logs = path.join(options.output, 'logs');
  fs.mkdirSync(logs, {recursive: true});
  const plan = buildPlan(options), checks = [{name: 'dependency-prepare', log: 'logs/dependency-prepare.log', exit_code: null}, ...plan];
  const save = () => json(path.join(options.output, 'checks.json'), checks);
  const environment = {
    schema: 2, captured_at: new Date().toISOString(), head: version('git', ['rev-parse', 'HEAD']),
    timezone: Intl.DateTimeFormat().resolvedOptions().timeZone, platform: process.platform, architecture: process.arch,
    rustc: version('rustc', ['-Vv']), cargo: version('cargo', ['-V']), rustfmt: version('rustfmt', ['--version']),
    clippy: version('cargo', ['clippy', '-V']), git: version('git', ['--version']), node: process.version,
    os: `${os.type()} ${os.release()}`, processor: os.cpus()[0]?.model, logical_processors: os.cpus().length,
    physical_memory_bytes: os.totalmem(), hardware_scope: 'Node OS metadata; Windows CIM details are not collected',
    lock_sha256: hash(fs.readFileSync(path.join(ROOT, 'Cargo.lock'))),
    lock_lf_sha256: hash(fs.readFileSync(path.join(ROOT, 'Cargo.lock'), 'utf8').replaceAll('\r\n', '\n')),
    manifest_sha256: hash(fs.readFileSync(path.join(ROOT, 'Cargo.toml'))), toolchain_sha256: hash(fs.readFileSync(path.join(ROOT, 'rust-toolchain.toml'))),
    lgui_source: fs.readFileSync(path.join(ROOT, 'Cargo.lock'), 'utf8').split(/\r?\n/).filter(line => line.includes('source = "git+https://github.com/lius-new/lgui')),
    inherited_git_autocrlf: version('git', ['config', '--show-origin', '--get', 'core.autocrlf']),
    perf_samples: options.samples, performance_set: options.performance, benchmarks: options.benchmarks.map(item => item.id),
    warmup_samples_per_filter: 1, modes: {checks: options.checks, isolated: options.isolated, native_smoke: options.nativeSmoke},
  };
  json(path.join(options.output, 'environment.json'), environment);
  fs.writeFileSync(path.join(options.output, 'working-tree.txt'), version('git', ['status', '--porcelain']) + '\n');
  const controlledConfig = path.join(options.output, 'controlled-git.config');
  fs.writeFileSync(controlledConfig, '[core]\n\tautocrlf = false\n\teol = lf\n');
  save();
  try {
    const started = performance.now();
    try {
      const prepared = await prepare({offline: options.offline, skiaArchive: options.skiaArchive});
      checks[0].exit_code = 0;
      checks[0].elapsed_seconds = (performance.now() - started) / 1000;
      fs.writeFileSync(path.join(logs, 'dependency-prepare.log'), JSON.stringify({cmake: prepared.cmake, skia: prepared.skia, target: prepared.target}, null, 2) + '\n');
      environment.skia_cache = prepared.skia;
      json(path.join(options.output, 'environment.json'), environment);
      save();
      const isolatedEnv = {...prepared.env, GIT_CONFIG_NOSYSTEM: '1', GIT_CONFIG_GLOBAL: controlledConfig};
      const failedBenchmarks = new Set();
      for (const check of plan) {
        if (signal?.aborted) break;
        if (failedBenchmarks.has(check.benchmark)) {check.error = 'Skipped after an earlier sample failed'; save(); continue;}
        const time = performance.now();
        Object.assign(check, await execute(check, {directory: options.output, env: check.isolated ? isolatedEnv : prepared.env, signal}));
        check.elapsed_seconds = (performance.now() - time) / 1000;
        if (check.exact_filter && check.exit_code === 0 && !validateExactTest(fs.readFileSync(path.join(options.output, check.log), 'utf8'), check.exact_filter)) {
          check.exit_code = 1;
          check.error = `Exact test did not pass once: ${check.exact_filter}`;
        }
        if (check.exit_code !== 0 && check.benchmark) failedBenchmarks.add(check.benchmark);
        save();
        console.log(`${check.name} exit=${check.exit_code}`);
      }
    } catch (error) {
      if (checks[0].exit_code === null) {
        checks[0].exit_code = 1;
        checks[0].error = error.message;
        fs.writeFileSync(path.join(logs, 'dependency-prepare.log'), error.stack + '\n');
      } else {
        fs.writeFileSync(path.join(logs, 'runner-error.log'), error.stack + '\n');
        checks.push({name: 'runner-error', log: 'logs/runner-error.log', exit_code: 1, error: error.message});
      }
    } finally {
      checks[0].elapsed_seconds ??= (performance.now() - started) / 1000;
      save();
    }
  } finally {
    save();
  }
  const report = generateReport(options.output);
  console.log(`Baseline report: ${path.join(options.output, 'measurements.md')}`);
  return report.complete ? 0 : 1;
}

if (path.resolve(process.argv[1] || '') === fileURLToPath(import.meta.url)) {
  const controller = new AbortController();
  const abort = () => controller.abort();
  process.once('SIGINT', abort);
  process.once('SIGTERM', abort);
  try {
    const options = optionsFromArgs(process.argv.slice(2));
    if (options.help) console.log('Usage: node scripts/baseline/run.mjs [--performance all|none|editor,ui,file,search,terminal] [--samples 1..20]\n  [--checks] [--isolated] [--native-smoke] [--offline] [--skia-archive <path>] [--output-directory <fresh-directory>]\nDefault: performance sampling only. Checks, isolated workspace tests and Windows native smoke are opt-in.');
    else process.exitCode = await runBaseline(options, {signal: controller.signal});
  } catch (error) {console.error(error.message); process.exitCode = 1;}
  finally {process.removeListener('SIGINT', abort); process.removeListener('SIGTERM', abort);}
}
