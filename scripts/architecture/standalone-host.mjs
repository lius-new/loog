import {prepareDependencies} from '../dependencies/ensure.mjs';
// Physical source-removal regression. The copied host has no language project
// and builds/tests without any language modules or grammar sources.
import fs from 'node:fs';
import path from 'node:path';
import assert from 'node:assert/strict';
import {fileURLToPath} from 'node:url';
import {execFileSync} from 'node:child_process';
export function hostFile(file) {
  return !file.startsWith('tests/language-runtime/') && !file.startsWith('language-packs/') && !file.startsWith('crates/language-packs/') &&
    !file.startsWith('assets/grammars/') && !file.startsWith('assets/languages/') &&
    !file.startsWith('assets/servers/') && !file.startsWith('assets/extensions/loom.lang.') &&
    file !== 'assets/extensions/registry.toml';
}
if(path.resolve(process.argv[1]??'')===fileURLToPath(import.meta.url)) {
  const root=path.resolve(path.dirname(fileURLToPath(import.meta.url)),'../..');
  // Fresh CI runners may still need ordinary target-specific dependencies for
  // Cargo metadata. Offline verification is available for a populated cache.
  const offline=process.argv.includes('--offline')?['--offline']:[];
  const env=(await prepareDependencies({offline:offline.length>0})).env;
  const target=path.join(root,'.tmp','host-without-language-sources-'+Date.now());
  const files=execFileSync('git',['ls-files','--cached','--others','--exclude-standard','-z'],{cwd:root,encoding:'utf8'}).split('\0').filter(Boolean);
  fs.mkdirSync(target,{recursive:true});
  for(const file of new Set(files)) {
    if(!hostFile(file)||!fs.existsSync(path.join(root,file)))continue;
    const out=path.resolve(target,file);assert.ok(out.startsWith(target+path.sep));
    fs.mkdirSync(path.dirname(out),{recursive:true});fs.copyFileSync(path.join(root,file),out);
  }
  assert.equal(fs.existsSync(path.join(target,'language-packs')),false);
  assert.equal(fs.existsSync(path.join(target,'assets/extensions/loom.lang.json')),false);
  // Avoid overwriting an editor the user may currently be running. Only the
  // output binary name changes; the full application sources are identical.
  const manifest=path.join(target,'Cargo.toml');
  fs.writeFileSync(manifest,fs.readFileSync(manifest,'utf8').replace('name = "Loom"','name = "LoomStandalone"'));
  const metadata=JSON.parse(execFileSync('cargo',['metadata','--manifest-path',manifest,'--locked',...offline,'--format-version','1'],{cwd:target,env,encoding:'utf8',maxBuffer:16*1024*1024}));
  assert.ok(!metadata.packages.some(p=>p.name.startsWith('loom-grammar-')||(p.name.startsWith('tree-sitter-')&&p.name!=='tree-sitter-language')));
  const common=['--manifest-path',manifest,'--locked',...offline,'--target-dir',path.join(root,'target')];
  execFileSync('cargo',['fmt','--manifest-path',manifest,'--all','--','--check'],{cwd:target,env,stdio:'inherit'});
  execFileSync('cargo',['build',...common],{cwd:target,env,stdio:'inherit'});
  execFileSync('cargo',['test','--workspace','--no-fail-fast',...common,...(process.platform==='win32'?['--','--test-threads=1']:[])],{cwd:target,env,stdio:'inherit'});
  console.log('Standalone host built and tested without language sources: '+target);
}
