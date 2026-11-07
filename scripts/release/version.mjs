// Materialize tag metadata only in the release working copy. Never commit it.
import fs from 'node:fs';
import path from 'node:path';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {parseArgs} from 'node:util';
import {ROOT,validateVersion} from './notes.mjs';

function blocks(text) {
  const headers = [...text.matchAll(/^\[[^\r\n]+\][ \t]*(?:#[^\r\n]*)?\r?$/gm)];
  return headers.map((header,index) => ({start:header.index,end:headers[index + 1]?.index ?? text.length,
    header:header[0].trim(),text:text.slice(header.index,headers[index + 1]?.index)}));
}
const field = (section,name) => new RegExp('^[ \t]*' + name + '[ \t]*=[ \t]*"([^"\r\n]+)"','m').exec(section)?.[1];
function packageBlock(text) {
  const matches = blocks(text).filter(block => block.header === '[package]' && field(block.text,'name') === 'loog');
  if (matches.length !== 1) throw Error('Expected one explicit Loog package in Cargo.toml');
  return matches[0];
}
export function manifestVersion(text) {
  const version = field(packageBlock(text).text,'version');validateVersion(version);return version;
}
function replaceVersion(text,block,version) {
  if (!field(block.text,'version')) throw Error('Missing explicit package version');
  const changed = block.text.replace(/^([ \t]*version[ \t]*=[ \t]*")[^"]+(".*)$/m,'$1' + version + '$2');
  return text.slice(0,block.start) + changed + text.slice(block.end);
}
export function versionFiles(manifest,lock,version) {
  validateVersion(version);
  const base = manifestVersion(manifest);
  const matches = blocks(lock).filter(block => block.header === '[[package]]' && field(block.text,'name') === 'loog' && !/^[ \t]*source[ \t]*=/m.test(block.text));
  if (matches.length !== 1 || field(matches[0].text,'version') !== base) throw Error('Cargo.lock Loog entry does not match the source manifest');
  return {manifest:replaceVersion(manifest,packageBlock(manifest),version),lock:replaceVersion(lock,matches[0],version)};
}
export function stampVersion({root = ROOT,version,sha}) {
  validateVersion(version);
  if (!/^[a-f0-9]{40}(?:[a-f0-9]{24})?$/i.test(sha || '')) throw Error('A verified release SHA is required before version generation');
  const git = args => execFileSync('git',args,{cwd:root,encoding:'utf8',stdio:['ignore','pipe','pipe']});
  if (git(['rev-parse','HEAD']).trim() !== sha) throw Error('Release checkout does not match the verified SHA');
  const baseManifest = git(['show',sha + ':Cargo.toml']), baseLock = git(['show',sha + ':Cargo.lock']);
  const generated = versionFiles(baseManifest,baseLock,version);
  const files = [{name:'Cargo.toml',base:baseManifest,text:generated.manifest},{name:'Cargo.lock',base:baseLock,text:generated.lock}];
  const changed = git(['diff','--name-only','-z','HEAD','--']).split('\0').filter(Boolean);
  if (changed.some(file => !files.some(allowed => allowed.name === file)) || git(['ls-files','--others','--exclude-standard','-z'])) throw Error('Release checkout contains unrelated changes');
  git(['diff','--cached','--quiet']);
  for (const file of files) {
    file.path = path.join(root,file.name);file.current = fs.readFileSync(file.path,'utf8');
    const normalize = text => text.replaceAll('\r\n','\n');
    if (normalize(file.current) !== normalize(file.base) && normalize(file.current) !== normalize(file.text)) throw Error('Unexpected release metadata changes: ' + file.name);
    // Git may check out the same blob with CRLF on Windows. Preserve that
    // checkout's line endings while comparing against the verified Git blob.
    file.text = normalize(file.text).replaceAll('\n',file.current.includes('\r\n') ? '\r\n' : '\n');
  }
  try {for (const file of files) if (file.current !== file.text) fs.writeFileSync(file.path,file.text);}
  catch (error) {for (const file of files) {try {fs.writeFileSync(file.path,file.current);} catch {}} throw error;}
  return {version,sha};
}
if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const {values} = parseArgs({options:{version:{type:'string'},sha:{type:'string'},'read-env':{type:'boolean',default:false}}});
    if (values['read-env']) {
      const version = manifestVersion(fs.readFileSync(path.join(ROOT,'Cargo.toml'),'utf8'));
      fs.appendFileSync(process.env.GITHUB_ENV,'LOOG_VERSION=' + version + '\n');
    } else console.log(JSON.stringify(stampVersion({version:values.version,sha:values.sha})));
  } catch (error) {console.error(error.message);process.exitCode = 1;}
}
