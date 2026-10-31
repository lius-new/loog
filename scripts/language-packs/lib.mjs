import fs from 'node:fs';
import path from 'node:path';
import {createHash, randomUUID} from 'node:crypto';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {downloadVerified} from '../lib/download.mjs';
import {gunzipSync} from 'node:zlib';
import {parse, stringify} from 'smol-toml';
import * as tar from 'tar';

// The language server pipeline (scripts/lsp-servers) shares this module's pinned
// download, archive and TOML helpers instead of repeating them.
export {parse as parseToml, stringify as stringifyToml, tar};

export const ROOT = path.resolve(fileURLToPath(new URL('../../', import.meta.url)));
export const RECIPES = path.join(ROOT, 'scripts/language-packs/build.toml');
export const CATALOG = path.join(ROOT, 'assets/extensions/registry.toml');
export const WORK = path.join(ROOT, '.packaging/language-packs');
export const ID = /^[a-z][a-z0-9_-]*$/;
const GRAMMAR = /^[a-z][a-z0-9_]*$/;
export const HASH = /^[a-f0-9]{64}$/;
export const readToml = file => parse(fs.readFileSync(file, 'utf8'));
export const hash = value => createHash('sha256').update(value).digest('hex');
export const fileHash = file => hash(fs.readFileSync(file));
export function canonical(value) {
  if(Array.isArray(value))return value.map(canonical);
  if(value && typeof value==='object')return Object.fromEntries(Object.keys(value).sort().map(key=>[key,canonical(value[key])]));
  return value;
}
export function assert(condition, message) { if (!condition) throw Error(message); }
export function relative(value, label = 'path') {
  assert(typeof value === 'string' && value && !value.startsWith('/') && !/[\\:\0]/.test(value)
    && !value.split('/').includes('..'), `Invalid ${label}: ${value}`);
  return value;
}
export function inside(root, value) {
  const target = path.resolve(root, relative(value));
  const actual = fs.realpathSync(target), base = fs.realpathSync(root);
  assert(actual === base || actual.startsWith(base + path.sep), `Path escapes source: ${value}`);
  return actual;
}
export function removeWork(target, root = WORK) {
  const base = path.resolve(root), absolute = path.resolve(target);
  assert(absolute.startsWith(base + path.sep), `Refusing to remove outside work directory: ${absolute}`);
  fs.rmSync(absolute, {recursive:true, force:true});
}
export function writeAtomic(file, value) {
  fs.mkdirSync(path.dirname(file), {recursive:true});
  const temporary = `${file}.${randomUUID()}.tmp`;
  try { fs.writeFileSync(temporary, value); fs.renameSync(temporary, file); }
  finally { fs.rmSync(temporary, {force:true}); }
}
export function run(command, args, options = {}) {
  return execFileSync(command, args, {cwd:ROOT, encoding:'utf8', timeout:600000, maxBuffer:32 * 1024 * 1024, ...options});
}
export function files(root) {
  const result = [];
  const walk = directory => {
    for (const entry of fs.readdirSync(directory, {withFileTypes:true}).sort((a,b) => a.name.localeCompare(b.name, 'en'))) {
      const full = path.join(directory, entry.name);
      if (entry.isDirectory()) walk(full);
      else { assert(entry.isFile(), `Package may contain only files and directories: ${full}`); result.push(path.relative(root, full).split(path.sep).join('/')); }
    }
  };
  walk(root);
  return result.sort();
}
export function inputDigest(recipes, root = ROOT) {
  const local = {};
  for (const [id, source] of Object.entries(recipes.sources)) {
    if (source.directory) {
      const directory = inside(root, source.directory);
      local[id] = Object.fromEntries(files(directory).map(file => [file, fileHash(path.join(directory, file))]));
    }
  }
  for (const pkg of Object.values(recipes.packages)) {
    for (const patch of pkg.patches || []) local[patch] = fileHash(inside(root, patch));
    for (const grammar of Object.values(pkg.grammars)) local[grammar.sample] = fileHash(inside(root, grammar.sample));
  }
  return hash(JSON.stringify(canonical({recipes, local})));
}

export function validateRecipes(data, {root = ROOT} = {}) {
  assert(data.schema === 1 && data.sources && data.packages && data.tools, 'Expected recipe schema 1, tools, sources and packages');
  assert(/^\d+\.\d+\.\d+$/.test(data.tools['cli-version']), 'Pin an exact tree-sitter CLI version');
  assert([14,15].includes(data.tools['generate-abi']), 'generate-abi must be 14 or 15');
  for (const kind of ['cli','wasi','binaryen']) {
    assert(Object.keys(data.tools[kind] || {}).length, `Missing ${kind} toolchain`);
    for (const spec of Object.values(data.tools[kind])) {
      assert(new URL(spec.url).protocol === 'https:' && HASH.test(spec.sha256), `Pin ${kind} URL and SHA-256`);
      assert(['gzip','tar.gz'].includes(spec.format), `Invalid ${kind} archive format`);
    }
  }
  for (const [id, source] of Object.entries(data.sources)) {
    assert(ID.test(id), `Invalid source id: ${id}`);
    assert(Boolean(source.directory) !== Boolean(source.repository), `Source ${id} requires either directory or repository`);
    if (source.directory) inside(root, source.directory);
    else {
      assert(/^https:\/\/github\.com\/[\w.-]+\/[\w.-]+$/.test(source.repository), `Expected GitHub source URL: ${id}`);
      assert(/^[a-f0-9]{40}$/.test(source.revision), `Pin a complete commit SHA: ${id}`);
      assert(HASH.test(source.sha256), `Pin source archive SHA-256: ${id}`);
      if (source.url) assert(new URL(source.url).protocol === 'https:', `Invalid source archive URL: ${id}`);
    }
    assert(Array.isArray(source.licenses) && source.licenses.length, `Missing license paths: ${id}`);
    source.licenses.forEach(file => relative(file));
    for (const dep of source.dependencies || []) {
      assert(data.sources[dep.source], `Unknown dependency source: ${dep.source}`); relative(dep.path);
    }
  }
  const grammarNames = new Set(), suffixes = new Map();
  for (const [id, pkg] of Object.entries(data.packages)) {
    assert(ID.test(id) && typeof pkg.name === 'string' && pkg.name, `Invalid package: ${id}`);
    assert(Object.keys(pkg.grammars || {}).length, `Package has no grammars: ${id}`);
    for (const patch of pkg.patches || []) inside(root, patch);
    for (const [name, grammar] of Object.entries(pkg.grammars)) {
      assert(GRAMMAR.test(name) && !grammarNames.has(name), `Invalid or duplicate grammar: ${name}`); grammarNames.add(name);
      assert(data.sources[grammar.source], `Unknown source for ${name}`); relative(grammar.path || '.');
      if(grammar.prebuilt) {relative(grammar.prebuilt);assert(!grammar.generate,`Cannot generate and use prebuilt for ${name}`);}
      for(const support of grammar.support||[]) {assert(data.sources[support.source] && grammar.optimize===false,`Support sources require the WASI driver: ${name}`);relative(support.path);assert(support.path.endsWith('.c'),`Expected C support source: ${name}`);}
      for(const [from,to]of Object.entries(grammar['capture-map']||{}))assert(/^[\w.-]+$/.test(from)&&/^[\w.-]+$/.test(to),`Invalid capture adaptation: ${name}`);
      assert(Array.isArray(grammar['file-types']), `Missing file-types: ${name}`);
      for (const suffix of grammar['file-types']) {
        assert(typeof suffix === 'string' && suffix && !/[\\/\0]/.test(suffix), `Invalid file type: ${suffix}`);
        const key = suffix.toLowerCase(); assert(!suffixes.has(key), `Conflicting file type ${suffix}: ${suffixes.get(key)} / ${id}`); suffixes.set(key, id);
      }
      assert(Array.isArray(grammar.highlights) && grammar.highlights.length, `Missing highlights: ${name}`);
      for (const kind of ['highlights','injections']) for (const query of grammar[kind] || []) {
        assert(data.sources[query.source || grammar.source], `Unknown query source: ${name}`); relative(query.path);
      }
      assert(typeof grammar.sample === 'string' && fs.statSync(inside(root, grammar.sample)).isFile(), `Missing sample for ${name}`);
    }
  }
  return data;
}
export const loadRecipes = (file = RECIPES) => validateRecipes(readToml(file));
export function selectPackages(recipes, options) {
  const ids = options.packages ? options.packages.split(',') : Object.keys(recipes.packages);
  assert(ids.length && new Set(ids).size === ids.length, 'Select at least one package without duplicates');
  ids.forEach(id => assert(recipes.packages[id], `Unknown package: ${id}`));
  return ids.sort();
}

export async function fetchPinned(url, expected, destination, {offline = false, ...options} = {}) {
  if (fs.existsSync(destination) && fileHash(destination) === expected) return destination;
  assert(!offline, `Missing or corrupt offline input: ${destination}`);
  return downloadVerified({url, destination, sha256:expected}, options);
}
export async function extractArchive(archive, destination) {
  fs.mkdirSync(destination, {recursive:true});
  const links = [];
  await tar.x({file:archive, cwd:destination, strict:true, filter:(name, entry) => {
    relative(name.replace(/\/$/, ''));
    assert(['File','Directory','SymbolicLink','Link'].includes(entry.type), `Unsupported archive entry: ${name}`);
    if (entry.linkpath) {
      assert(!path.posix.isAbsolute(entry.linkpath) && !/[\\:\0]/.test(entry.linkpath), `Unsafe archive link: ${name}`);
      const target = path.posix.normalize(path.posix.join(entry.type === 'Link' ? '' : path.posix.dirname(name), entry.linkpath));
      relative(target);
      links.push({name,target});
      return false;
    }
    return true;
  }});
  // Materialize internal links as copies, also on Windows without symlink rights.
  for (let pending = links; pending.length;) {
    const next = [];
    for (const link of pending) {
      const target = path.join(destination,link.target), output = path.join(destination,link.name);
      if (!fs.existsSync(target)) {next.push(link);continue;}
      const actual = inside(destination,link.target);
      fs.mkdirSync(path.dirname(output),{recursive:true});
      fs.cpSync(actual,output,{recursive:true,dereference:true});
    }
    assert(next.length < pending.length, 'Unresolvable archive links'); pending = next;
  }
}
export async function createArchive(directory, output) {
  await tar.c({file:output, cwd:path.dirname(directory), gzip:true, portable:true,
    mtime:new Date(0), noDirRecurse:true}, [path.basename(directory), ...files(directory).map(file => path.posix.join(path.basename(directory), file))]);
}
function oneRoot(directory) {
  const entries = fs.readdirSync(directory, {withFileTypes:true});
  return entries.length === 1 && entries[0].isDirectory() ? path.join(directory, entries[0].name) : directory;
}
export async function prepareTools(recipes, options = {}) {
  const platform = `${process.platform}-${process.arch}`, result = {};
  for (const kind of ['cli','wasi','binaryen']) {
    const spec = recipes.tools[kind][platform]; assert(spec, `No pinned ${kind} toolchain for ${platform}`);
    const base = path.join(options.work || WORK, 'tools', spec.sha256);
    const archive = await fetchPinned(spec.url, spec.sha256, path.join(base, `input.${spec.format}`), options);
    const content = path.join(base, 'content');
    if (!fs.existsSync(path.join(base, 'complete'))) {
      removeWork(content, options.work || WORK); fs.mkdirSync(content, {recursive:true});
      if (spec.format === 'gzip') fs.writeFileSync(path.join(content, process.platform === 'win32' ? 'tree-sitter.exe' : 'tree-sitter'), gunzipSync(fs.readFileSync(archive)));
      else await extractArchive(archive, content);
      fs.writeFileSync(path.join(base, 'complete'), spec.sha256);
    }
    result[kind] = oneRoot(content);
  }
  result.executable = path.join(result.cli, process.platform === 'win32' ? 'tree-sitter.exe' : 'tree-sitter');
  if (process.platform !== 'win32') fs.chmodSync(result.executable, 0o755);
  assert(run(result.executable, ['--version']).trim().split(/\s+/).slice(0,2).join(' ') === `tree-sitter ${recipes.tools['cli-version']}`, 'Tree-sitter CLI version mismatch');
  result.env = {...process.env, TREE_SITTER_WASI_SDK_PATH:result.wasi,
    TREE_SITTER_BINARYEN_PATH:result.binaryen, TREE_SITTER_DIR:path.join(options.work || WORK,'cli-config'),
    TREE_SITTER_LIBDIR:path.join(options.work || WORK,'cli-cache'), NO_COLOR:'1'};
  return result;
}

export function sourcePreparer(recipes, options = {}) {
  const promises = new Map(), work = options.work || WORK;
  const prepare = (id, ancestors = []) => {
    assert(!ancestors.includes(id), `Cyclic source dependencies: ${[...ancestors,id].join(' -> ')}`);
    if (promises.has(id)) return promises.get(id);
    const promise = (async () => {
      const spec = recipes.sources[id];
      let directory;
      if (spec.directory) directory = inside(options.root || ROOT, spec.directory);
      else {
        const repo = spec.repository.slice('https://github.com/'.length);
        const url = spec.url || `https://codeload.github.com/${repo}/tar.gz/${spec.revision}`;
        const archive = await fetchPinned(url, spec.sha256, path.join(work, 'downloads', spec.sha256 + '.tar.gz'), options);
        const base = path.join(work, 'sources', spec.sha256), content = path.join(base, 'content');
        if (!fs.existsSync(path.join(base, 'complete'))) {
          removeWork(content, work); await extractArchive(archive, content); fs.writeFileSync(path.join(base, 'complete'), spec.sha256);
        }
        directory = oneRoot(content);
      }
      if (spec.dependencies?.length) {
        const assembled = path.join(work, 'prepared-sources', id + '-' + hash(JSON.stringify(spec)));
        removeWork(assembled, work); fs.cpSync(directory, assembled, {recursive:true, dereference:true});
        for (const dep of spec.dependencies) {
          const dependency = await prepare(dep.source, [...ancestors,id]);
          const target = path.join(assembled, relative(dep.path));
          assert(!fs.existsSync(target), `Dependency destination already exists: ${target}`);
          fs.cpSync(dependency, target, {recursive:true, dereference:true});
        }
        directory = assembled;
      }
      return directory;
    })();
    promises.set(id, promise); return promise;
  };
  return prepare;
}

export async function assemblePackage(recipes, id, prepare, tools, options = {}) {
  const work = options.work || WORK, root = options.root || ROOT, pkg = recipes.packages[id];
  const directory = path.join(work, 'stage', id); removeWork(directory, work); fs.mkdirSync(directory, {recursive:true});
  const grammars = [], usedSources = new Set();
  const readQuery = async (reference, source) => {
    const id = reference.source || source; usedSources.add(id);
    return fs.readFileSync(inside(await prepare(id), reference.path), 'utf8');
  };
  for (const [name, grammar] of Object.entries(pkg.grammars)) {
    usedSources.add(grammar.source);
    for(const support of grammar.support||[])usedSources.add(support.source);
    let source = await prepare(grammar.source), grammarPath = grammar.path || '.';
    if (grammar.generate) {
      const generated = path.join(work, 'generated', `${id}-${name}`); removeWork(generated, work);
      fs.cpSync(source, generated, {recursive:true, dereference:true}); source = generated;
      const cwd = inside(source, grammarPath);
      run(tools.executable, ['generate', '--abi', String(recipes.tools['generate-abi']), ...(fs.existsSync(path.join(cwd,'src/grammar.json')) ? ['src/grammar.json'] : [])], {cwd, env:tools.env});
    }
    const output = path.join(directory, `tree-sitter-${name}.wasm`);
    if(!grammar.prebuilt)assert(fs.existsSync(path.join(inside(source, grammarPath), 'src/parser.c')), `Missing parser.c for ${name}; explicitly enable generate`);
    const key=moduleFingerprint(recipes,name,grammar,root),cache=path.join(work,'modules',key+'.wasm'),receipt=cache+'.sha256';
    if(fs.existsSync(cache)&&fs.existsSync(receipt)&&fileHash(cache)===fs.readFileSync(receipt,'utf8').trim())fs.copyFileSync(cache,output);
    else {
      if(grammar.prebuilt)fs.copyFileSync(inside(source,relative(grammar.prebuilt)),output);
      else if(grammar.optimize===false) {
        // Same WASI shared-module ABI as the CLI; omit only the optional Binaryen pass.
        const cwd=path.join(inside(source,grammarPath),'src');
        const clang=path.join(tools.wasi,'bin',process.platform==='win32'?'clang.exe':'clang');
        const scanner=fs.existsSync(path.join(cwd,'scanner.c'))?['scanner.c']:[];
        const support=await Promise.all((grammar.support||[]).map(async ref=>inside(await prepare(ref.source),ref.path)));
        run(clang,['--target=wasm32-wasip1','-o',output,'-fPIC','-shared','--no-wasm-opt','-Os',
          `-Wl,--export=tree_sitter_${name}`,'-Wl,--allow-undefined','-Wl,--no-entry','-nostdlib','-fno-exceptions','-fvisibility=hidden','-I','.','parser.c',...scanner,...support],{cwd,env:tools.env});
      } else run(tools.executable, ['build', '--wasm', '--output', output, inside(source, grammarPath)], {env:tools.env});
      fs.mkdirSync(path.dirname(cache),{recursive:true});fs.copyFileSync(output,cache);writeAtomic(receipt,fileHash(cache)+'\n');
    }
    const metadata = {name, path:'.', 'file-types':grammar['file-types']};
    for (const kind of ['highlights','injections']) {
      const references = grammar[kind] || []; if (!references.length) continue;
      const text = rewriteCaptures((await Promise.all(references.map(ref => readQuery(ref, grammar.source)))).join('\n'), kind === 'highlights' ? grammar['capture-map'] : {});
      const file = `queries/${name}/${kind}.scm`;
      fs.mkdirSync(path.dirname(path.join(directory,file)), {recursive:true}); fs.writeFileSync(path.join(directory,file), text);
      metadata[kind] = file;
    }
    grammars.push(metadata);
  }
  const licenseSources = new Set();
  const addSource = id => { if (licenseSources.has(id)) return; licenseSources.add(id); for (const dep of recipes.sources[id].dependencies || []) addSource(dep.source); };
  usedSources.forEach(addSource);
  for (const id of [...licenseSources].sort()) {
    const source = await prepare(id);
    for (const [i, file] of recipes.sources[id].licenses.entries()) {
      const output = path.join(directory, 'licenses', `${id}-${i}-${path.basename(file)}`);
      fs.mkdirSync(path.dirname(output), {recursive:true}); fs.copyFileSync(inside(source, file), output);
    }
  }
  fs.writeFileSync(path.join(directory,'tree-sitter.json'), JSON.stringify({grammars}, null, 2) + '\n');
  for (const patch of pkg.patches || []) applyQueryPatch(directory, inside(root,patch));
  fs.writeFileSync(path.join(directory,'PROVENANCE.md'), `# ${pkg.name}\n\nProduced by Loom.\n\nCLI: ${recipes.tools['cli-version']}\n\n` +
    [...licenseSources].sort().map(id => {const s=recipes.sources[id]; return `- ${id}: ${s.repository ? `${s.repository} @ ${s.revision}` : `Loom repository / ${s.directory}`}\n`;}).join('') +
    (pkg.patches?.length ? `\nApplied patches: ${pkg.patches.join(', ')}\n` : '')+
    Object.entries(pkg.grammars).filter(([,g])=>g['capture-map']).map(([name,g])=>`\nCapture adaptations (${name}): ${JSON.stringify(g['capture-map'])}\n`).join(''));
  for(const [name,g]of Object.entries(pkg.grammars))if(g.prebuilt)fs.appendFileSync(path.join(directory,'PROVENANCE.md'),`\nPrebuilt module (${name}): ${g.source}/${g.prebuilt}, pinned by its source archive.\n`);
  return directory;
}

export function moduleFingerprint(recipes,name,grammar,root=ROOT) {
  const sources={};
  const visit=id=>{
    if(sources[id])return;
    const spec=recipes.sources[id];sources[id]={...spec};
    if(spec.directory){const directory=inside(root,spec.directory);sources[id].files=Object.fromEntries(files(directory).map(file=>[file,fileHash(path.join(directory,file))]));}
    for(const dep of spec.dependencies||[])visit(dep.source);
  };
  visit(grammar.source);
  for(const support of grammar.support||[])visit(support.source);
  return hash(JSON.stringify(canonical({name,source:grammar.source,path:grammar.path||'.',generate:Boolean(grammar.generate),prebuilt:grammar.prebuilt||null,...(grammar.optimize===false?{optimize:false}:{}),...(grammar.support?.length?{support:grammar.support}:{}),tools:recipes.tools,sources})));
}

export function rewriteCaptures(text, mapping = {}) {
  // Captures inside comments and quoted query strings are literal source text.
  return text.replace(/;[^\n]*|"(?:\\.|[^"\\])*"|@([\w.-]+)/g, (token,name) => name && Object.hasOwn(mapping,name) ? '@'+mapping[name] : token);
}
export function applyQueryPatch(directory, file) {
  const patch=fs.readFileSync(file,'utf8');
  const paths=[...patch.matchAll(/^(?:---|\+\+\+) ([^\t\n]+)(?:\t[^\n]*)?$/gm)].map(m=>m[1]);
  assert(paths.length && paths.every(name=>{
    if(name==='/dev/null')return true;
    const value=name.replace(/^[ab]\//,''); relative(value);
    return value.startsWith('queries/') && value.endsWith('.scm');
  }), 'Patches may only modify package queries');
  run('git',['apply','--unsafe-paths','--whitespace=error',`--directory=${directory.replaceAll('\\','/')}`,file]);
}

export function packageRecord(id, pkg, archive) {
  return {name:pkg.name, asset:`${id}.tar.gz`, sha256:fileHash(archive), size:fs.statSync(archive).size,
    grammars:Object.fromEntries(Object.entries(pkg.grammars).map(([name,g]) => [name, {'file-types':g['file-types']}]))};
}
export function makeCatalog(report, {repository, release}) {
  assert(/^[\w.-]+\/[\w.-]+$/.test(repository), 'Invalid release repository');
  assert(/^language-packs-[a-zA-Z0-9][a-zA-Z0-9._-]*$/.test(release), 'Use a language-packs-* resource tag');
  assert(report.complete && !report.failures?.length, 'Cannot publish an incomplete batch');
  return {schema:1, distribution:{repository}, packages:Object.fromEntries(Object.entries(report.packages).map(([id,pkg]) => [id,{...pkg,release}]))};
}
export function validateCatalog(data) {
  assert(data.schema === 1 && /^[\w.-]+\/[\w.-]+$/.test(data.distribution?.repository), 'Invalid publication manifest');
  assert(Object.keys(data).every(key=>['schema','distribution','packages'].includes(key)), 'Build recipes do not belong in the publication manifest');
  const grammars=new Set(),suffixes=new Set();
  for (const [id,pkg] of Object.entries(data.packages || {})) {
    assert(typeof pkg.name==='string'&&pkg.name,`Missing package name: ${id}`);
    assert(Object.keys(pkg).every(key=>['name','asset','release','sha256','size','grammars'].includes(key)), `Unexpected publication field: ${id}`);
    assert(ID.test(id) && pkg.asset === `${id}.tar.gz`, `Invalid release asset: ${id}`);
    assert(/^language-packs-[a-zA-Z0-9][a-zA-Z0-9._-]*$/.test(pkg.release), `Invalid resource tag: ${id}`);
    assert(HASH.test(pkg.sha256) && Number.isSafeInteger(pkg.size) && pkg.size > 0, `Invalid release digest/size: ${id}`);
    assert(Object.keys(pkg.grammars || {}).length, `Missing released grammar: ${id}`);
    for(const [name,grammar]of Object.entries(pkg.grammars)) {
      assert(GRAMMAR.test(name) && !grammars.has(name) && Object.keys(grammar).every(key=>key==='file-types') && Array.isArray(grammar['file-types']), `Invalid published grammar: ${name}`);grammars.add(name);
      for(const suffix of grammar['file-types']){assert(typeof suffix==='string'&&suffix&&!/[\\/\0]/.test(suffix)&&!suffixes.has(suffix.toLowerCase()),`Conflicting published file type: ${suffix}`);suffixes.add(suffix.toLowerCase());}
    }
  }
  return data;
}
export function downloadUrl(catalog, id) {
  const pkg = catalog.packages[id]; assert(pkg, `Language package is missing: ${id}`);
  return `https://github.com/${catalog.distribution.repository}/releases/download/${encodeURIComponent(pkg.release)}/${pkg.asset}`;
}
export function checkArtifacts(catalog, directory) {
  validateCatalog(catalog);
  for (const [id,pkg] of Object.entries(catalog.packages)) {
    const file = path.join(directory, pkg.asset);
    assert(fs.existsSync(file) && fs.statSync(file).size === pkg.size && fileHash(file) === pkg.sha256, `Release artifact mismatch: ${id}`);
  }
}
export const toml = data => '# Generated from verified language-package artifacts.\n' + stringify(data);
export function options(argv) {
  const result = {};
  for (let i=0;i<argv.length;i++) {
    const arg=argv[i];
    if (['--all','--offline','--help','--draft-only'].includes(arg)) result[arg.slice(2)] = true;
    else if (['--packages','--recipes','--work','--release','--repository','--candidate','--catalog'].includes(arg)) {
      assert(argv[i+1] && !argv[i+1].startsWith('--'), `Missing value: ${arg}`); result[arg.slice(2)] = argv[++i];
    } else throw Error(`Unknown option: ${arg}`);
  }
  assert(!(result.all && result.packages), 'Use either --all or --packages');
  if (result.work) result.work = path.resolve(result.work);
  return result;
}
export function main(callback) { callback().catch(error => {console.error(error.message); process.exitCode=1;}); }
