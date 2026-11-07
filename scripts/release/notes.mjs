import fs from 'node:fs';
import path from 'node:path';
import {execFileSync} from 'node:child_process';
import {fileURLToPath} from 'node:url';
import {parseArgs} from 'node:util';

export const ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '../..');
const VERSION = /^(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)\.(?:0|[1-9]\d*)(?:-[0-9A-Za-z]+(?:[.-][0-9A-Za-z]+)*)?$/;
const GROUPS = ['Added', 'Improved', 'Changed', 'Removed', 'Fixed', 'Upgrade notes'];

export function validateVersion(version) {
  if (!VERSION.test(version ?? '')) throw new Error(`Invalid release version: ${version}`);
}

export function git(root, args) {
  return execFileSync('git', ['-C', root, ...args], {encoding: 'utf8', stdio: ['ignore', 'pipe', 'pipe']}).trim();
}

export function commitId(root, ref) {
  return git(root, ['rev-parse', '--verify', '--end-of-options', `${ref}^{commit}`]);
}

export function parseReleaseNotes(body) {
  const lines = body.replaceAll('\r\n', '\n').split('\n');
  let start = -1;
  let fence;
  for (let i = 0; i < lines.length; i++) {
    const marker = lines[i].match(/^\s*(`{3,}|~{3,})/);
    if (marker) {
      if (!fence) fence = marker[1][0];
      else if (marker[1][0] === fence) fence = undefined;
      continue;
    }
    if (!fence && /^(?:#{1,6}\s+)?Release Notes:?\s*$/i.test(lines[i])) {
      start = i + 1;
      break;
    }
  }
  if (start < 0) return {present: false, entries: []};
  const entries = [];
  for (const line of lines.slice(start)) {
    if (!line.trim()) continue;
    if (/^(?:#{1,6}\s|[A-Za-z][A-Za-z-]*:\s|\s*-{3,}\s*$)/.test(line)) break;
    const bullet = line.match(/^[-*]\s+(.+)$/);
    if (bullet) entries.push(bullet[1].trim());
    else if (/^\s+\S/.test(line) && entries.length) entries[entries.length - 1] += ' ' + line.trim();
    else throw new Error(`Release Notes must contain English Added/Improved/Changed/Removed/Fixed/Upgrade bullets or - N/A: ${line}`);
  }
  if (!entries.length) throw new Error('Release Notes section is empty; use - N/A for internal changes.');
  return {
    present: true,
    entries: entries.filter(entry => !/^N\/A[.!]?$/i.test(entry)).map(entry => {
      if (/\p{Extended_Pictographic}/u.test(entry)) throw new Error('Release Notes must not contain icons.');
      const match = entry.match(/^(Added|Improved|Changed|Removed|Fixed|Upgrade|Breaking change)(?::\s*|\s+)(.+)$/i);
      if (!match) throw new Error(`Unsupported Release Notes entry: ${entry}`);
      const kind = match[1].toLowerCase();
      const group = GROUPS.find(group => group.toLowerCase() === kind) ?? 'Upgrade notes';
      return {group, text: group === 'Upgrade notes' ? match[2] : `${group} ${match[2]}`};
    }),
  };
}

export function generateReleaseNotes({root = ROOT, version, ref = 'HEAD', repo = 'lius-new/loog'}) {
  validateVersion(version);
  if (!/^[\w.-]+\/[\w.-]+$/.test(repo)) throw new Error(`Invalid GitHub repository: ${repo}`);
  const target = commitId(root, ref);
  const tags = git(root, ['tag', '--merged', target]).split('\n')
    .filter(tag => tag.startsWith('v') && VERSION.test(tag.slice(1)) && tag !== `v${version}`);
  const previousTag = tags.length
    ? git(root, ['describe', '--tags', '--abbrev=0', ...tags.flatMap(tag => ['--match', tag]), target])
    : null;
  const log = git(root, ['log', '--reverse', '--format=%H%x00%B%x00', previousTag ? `${previousTag}..${target}` : target]);
  const fields = log.split('\0');
  const groups = new Map(GROUPS.map(group => [group, []]));
  const seen = new Set();
  const missing = [];
  for (let i = 0; i + 1 < fields.length; i += 2) {
    const hash = fields[i].trim();
    const body = fields[i + 1];
    let parsed;
    try { parsed = parseReleaseNotes(body); }
    catch (error) { throw new Error(`${hash.slice(0, 7)}: ${error.message}`); }
    if (!parsed.present) missing.push({hash, subject: body.split('\n')[0]});
    for (const entry of parsed.entries) {
      const key = `${entry.group}:${entry.text.replace(/\s+/g, ' ')}`;
      if (seen.has(key)) continue;
      seen.add(key);
      groups.get(entry.group).push(`- ${entry.text} ([${hash.slice(0, 7)}](https://github.com/${repo}/commit/${hash}))`);
    }
  }
  const sections = [`# Loog v${version}`];
  for (const [group, entries] of groups) if (entries.length) sections.push(`## ${group}\n\n${entries.join('\n')}`);
  if (!seen.size) sections.push('No user-facing changes were recorded for this release.');
  const link = previousTag ? `https://github.com/${repo}/compare/${previousTag}...v${version}` : `https://github.com/${repo}/commits/v${version}`;
  sections.push(`**Full changelog**: [${previousTag ? `${previousTag}...v${version}` : `v${version}`}](${link})`);
  return {text: sections.join('\n\n') + '\n', previousTag, missing};
}

export function writeReleaseNotes(result, output) {
  if (result.missing.length) console.error(`Warning: ${result.missing.length} commit(s) have no Release Notes section and were omitted:\n${result.missing.map(commit => `  ${commit.hash.slice(0, 7)} ${commit.subject}`).join('\n')}`);
  if (output) {
    fs.mkdirSync(path.dirname(path.resolve(output)), {recursive: true});
    fs.writeFileSync(output, result.text);
  } else process.stdout.write(result.text);
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const {values} = parseArgs({options: {version: {type: 'string'}, ref: {type: 'string', default: 'HEAD'}, repo: {type: 'string', default: 'lius-new/loog'}, output: {type: 'string'}}});
    writeReleaseNotes(generateReleaseNotes(values), values.output);
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
