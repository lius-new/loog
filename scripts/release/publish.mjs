import fs from 'node:fs';
import path from 'node:path';
import {fileURLToPath} from 'node:url';
import {parseArgs} from 'node:util';
import {github as gh} from './github.mjs';
import {verifyAssets} from './assets.mjs';
import {ROOT, commitId, generateReleaseNotes, validateVersion, writeReleaseNotes} from './notes.mjs';

export async function createDraft({version, repo, notes}, run = gh) {
  const tag = `v${version}`;
  const view = () => run(['release', 'view', tag, '--repo', repo, '--json', 'isDraft', '--jq', '.isDraft']);
  let existing = await view();
  if (existing.status !== 0) {
    if (/HTTP (401|403)|permission denied|bad credentials/i.test(existing.stderr || existing.stdout) || !/release not found|^not found$|HTTP 404/i.test(existing.stderr || existing.stdout)) throw Error('Could not inspect release draft: ' + (existing.stderr || existing.stdout));
    const created = await run(['release', 'create', tag, '--repo', repo, '--draft', '--verify-tag', '--title', `Loog ${version}`, '--notes-file', notes]);
    if (created.status !== 0) {
      // Recover a competing manual creation without replacing its edited notes.
      existing = await view();
      if (existing.status !== 0) throw new Error(`Could not create release draft: ${created.stderr || created.stdout}`);
    } else existing = await view();
  }
  if (existing.status !== 0) throw new Error(`Could not verify release draft: ${existing.stderr || existing.stdout}`);
  if (existing.stdout.trim() !== 'true') throw new Error(`${tag} is already published; refusing to change its notes or assets.`);
}

// Single-writer release orchestration makes missing-draft polling unnecessary.
export async function uploadToDraft({version,repo,assets},run = gh) {
  const tag = 'v' + version;
  const existing = await run(['release','view',tag,'--repo',repo,'--json','isDraft','--jq','.isDraft']);
  if (existing.status !== 0) throw Error('Draft ' + tag + ' is unavailable; use --publish-draft to create and upload in order. ' + (existing.stderr || existing.stdout));
  if (existing.stdout.trim() !== 'true') throw Error(tag + ' is already published; refusing to change its notes or assets.');
  await upload({version,repo,assets},run);
}
async function upload({version,repo,assets},run) {
  const result = await run(['release','upload','v' + version,...assets,'--clobber','--repo',repo]);
  if (result.status !== 0) throw Error('Could not upload release assets: ' + (result.stderr || result.stdout));
}
export async function publishDraft(options,run = gh) {
  const prepared = prepareUpload(options);
  prepared.assets = verifyAssets(options.assets,options.version);
  const draft = prepareDraft(options);
  await createDraft(draft,run);
  await upload(prepared,run);
}

export function verifySource({root = ROOT, version, target = 'HEAD', repo}) {
  validateVersion(version);
  if (!/^[\w.-]+\/[\w.-]+$/.test(repo ?? '')) throw new Error(`Invalid GitHub repository: ${repo}`);
  const targetId = commitId(root, target);
  let tagId;
  try { tagId = commitId(root, `refs/tags/v${version}`); }
  catch { throw new Error(`Tag v${version} must already exist. This command does not create tags.`); }
  if (tagId !== targetId) throw new Error(`Tag v${version} does not point to the build commit.`);
  return targetId;
}

export function prepareDraft({root = ROOT, version, target = 'HEAD', repo, notes}) {
  const targetId = verifySource({root, version, target, repo});
  if (!notes) throw new Error('--notes is required when creating a draft.');
  const result = generateReleaseNotes({root, version, ref: targetId, repo});
  writeReleaseNotes(result, notes);
  return {version, repo, notes};
}

export function prepareUpload({root = ROOT, version, target = 'HEAD', repo, assets}) {
  verifySource({root, version, target, repo});
  const files = fs.readdirSync(assets, {withFileTypes: true}).filter(entry => entry.isFile()).map(entry => path.resolve(assets, entry.name)).sort();
  if (!files.length) throw new Error(`No release assets found in ${assets}`);
  return {version, repo, assets: files};
}

if (process.argv[1] && path.resolve(process.argv[1]) === fileURLToPath(import.meta.url)) {
  try {
    const {values} = parseArgs({options: {'publish-draft': {type:'boolean',default:false}, create: {type: 'boolean', default: false}, version: {type: 'string'}, target: {type: 'string', default: 'HEAD'}, repo: {type: 'string'}, assets: {type: 'string', default: 'dist'}, notes: {type: 'string'}}});
    if (values['publish-draft']) await publishDraft(values);
    else if (values.create) await createDraft(prepareDraft(values));
    else await uploadToDraft(prepareUpload(values));
    console.log(`Prepared draft v${values.version}. Review all platform packages and notes before publishing it on GitHub.`);
  } catch (error) { console.error(error.message); process.exitCode = 1; }
}
