import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import {execFileSync} from 'node:child_process';
import {manifestVersion,versionFiles,stampVersion} from './version.mjs';
import {verifySource} from './publish.mjs';
import {packageRelease} from './package.mjs';
const manifest = '[package]\nname = "loog"\nversion = "0.1.7"\nedition = "2024"\n\n[dependencies]\n';
const lock = '# generated lock\nversion = 4\n\n[[package]]\nname = "loog"\nversion = "0.1.7"\n';
function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(),'loog-tag-version-'));
  t.after(() => fs.rmSync(root,{recursive:true,force:true}));
  const git = (...args) => execFileSync('git',args,{cwd:root,encoding:'utf8',stdio:['ignore','pipe','pipe']}).trim();
  git('init','-b','main');git('config','user.name','Version tests');git('config','user.email','version@example.invalid');git('config','commit.gpgsign','false');
  fs.mkdirSync(path.join(root,'src'));
  fs.writeFileSync(path.join(root,'src/main.rs'),'fn main() { println!("{}", env!("CARGO_PKG_VERSION")); }\n');
  fs.writeFileSync(path.join(root,'Cargo.toml'),manifest);fs.writeFileSync(path.join(root,'Cargo.lock'),lock);
  fs.writeFileSync(path.join(root,'.gitignore'),'/target/\n');
  git('add','.');git('commit','-m','Product change');git('tag','v0.2.0');
  return {root,git,sha:git('rev-parse','HEAD')};
}
test('generation changes only the root package version and preserves dependencies, comments and CRLF', () => {
  const source = manifest.replaceAll('\n','\r\n');
  const locked = (lock + '\n[[package]]\nname = "other"\nversion = "0.1.7"\n').replaceAll('\n','\r\n');
  const result = versionFiles(source,locked,'0.2.0-beta.1');
  assert.equal(result.manifest,source.replace('version = "0.1.7"','version = "0.2.0-beta.1"'));
  assert.equal(result.lock,locked.replace('version = "0.1.7"','version = "0.2.0-beta.1"'));
  assert.equal(manifestVersion(result.manifest),'0.2.0-beta.1');
  assert.throws(() => versionFiles(manifest,lock.replace('0.1.7','0.1.6'),'0.2.0'),/does not match/);
  assert.throws(() => versionFiles(manifest,lock,'../bad'),/Invalid release version/);
});
test('a new version tag on an existing product commit needs no source-version commit', t => {
  const f = fixture(t);
  assert.equal(verifySource({root:f.root,repo:'test/loog',version:'0.2.0',target:'HEAD'}),f.sha);
  assert.equal(fs.readFileSync(path.join(f.root,'Cargo.toml'),'utf8'),manifest);
  stampVersion({root:f.root,version:'0.2.0',sha:f.sha});
  assert.equal(f.git('rev-parse','HEAD'),f.sha);
  assert.equal(f.git('rev-list','--count','HEAD'),'1');
  assert.equal(f.git('rev-parse','v0.2.0'),f.sha);
  assert.equal(f.git('diff','--cached','--name-only'),'');
  assert.deepEqual(f.git('diff','--name-only').split('\n').sort(),['Cargo.lock','Cargo.toml']);
  assert.equal(manifestVersion(fs.readFileSync(path.join(f.root,'Cargo.toml'),'utf8')),'0.2.0');
  stampVersion({root:f.root,version:'0.2.0',sha:f.sha});
  assert.equal(f.git('rev-list','--count','HEAD'),'1');
});
test('a real locked offline Cargo build embeds the tag version', t => {
  const f = fixture(t);stampVersion({root:f.root,version:'0.2.0',sha:f.sha});
  const locked = fs.readFileSync(path.join(f.root,'Cargo.lock'),'utf8');
  const output = execFileSync('cargo',['run','--offline','--locked','--quiet'],{cwd:f.root,encoding:'utf8',windowsHide:true,timeout:60000});
  assert.equal(output.trim(),'0.2.0');
  assert.equal(fs.readFileSync(path.join(f.root,'Cargo.lock'),'utf8'),locked);
  assert.equal(f.git('rev-parse','HEAD'),f.sha);
});
test('wrong commits, unrelated edits and unexpected Cargo modifications fail before metadata is written', t => {
  for (const mode of ['wrong-sha','dirty-source','dirty-manifest','untracked']) {
    const f = fixture(t);
    if (mode === 'dirty-source') fs.appendFileSync(path.join(f.root,'src/main.rs'),'// change\n');
    if (mode === 'dirty-manifest') fs.appendFileSync(path.join(f.root,'Cargo.toml'),'# unrelated change\n');
    if (mode === 'untracked') fs.writeFileSync(path.join(f.root,'extra.rs'),'// untracked');
    const before = fs.readFileSync(path.join(f.root,'Cargo.toml'),'utf8');
    assert.throws(() => stampVersion({root:f.root,version:'0.2.0',sha:mode === 'wrong-sha' ? 'f'.repeat(40) : f.sha}),/verified SHA|unrelated changes|Unexpected release metadata/);
    assert.equal(fs.readFileSync(path.join(f.root,'Cargo.toml'),'utf8'),before);
    assert.equal(fs.readFileSync(path.join(f.root,'Cargo.lock'),'utf8'),lock);
  }
});
test('version generation precedes setup and compile; a failed stamp prevents every build operation', () => {
  const phases = [];
  packageRelease({platform:'linux',version:'0.2.0',sha:'a'.repeat(40)},
    {stamp:options => {assert.equal(options.version,'0.2.0');phases.push('stamp');},run:(_command,args) => phases.push(args[1])});
  assert.deepEqual(phases,['stamp','setup','compile','package','bundle']);
  assert.throws(() => packageRelease({platform:'linux',version:'0.2.0'},
    {stamp:() => {throw Error('invalid source');},run:() => assert.fail('build started before version validation')}),/invalid source/);
});

test('Windows CRLF checkouts materialize versions without unrelated differences', t => {
  const f = fixture(t);
  f.git('config','core.autocrlf','true');
  // Model a fresh Windows text checkout explicitly; checkout-index can keep
  // already up-to-date files untouched despite a changed autocrlf setting.
  for (const name of ['Cargo.toml','Cargo.lock']) {
    const file = path.join(f.root,name);
    fs.writeFileSync(file,fs.readFileSync(file,'utf8').replaceAll('\n','\r\n'));
  }
  assert.ok(fs.readFileSync(path.join(f.root,'Cargo.toml'),'utf8').includes('\r\n'));
  stampVersion({root:f.root,version:'0.2.0',sha:f.sha});
  const text = fs.readFileSync(path.join(f.root,'Cargo.toml'),'utf8');
  assert.ok(text.includes('version = "0.2.0"\r\n'));
  assert.deepEqual(f.git('diff','--name-only').split('\n').sort(),['Cargo.lock','Cargo.toml']);
  assert.equal(f.git('rev-parse','HEAD'),f.sha);
});
