import test from 'node:test';
import assert from 'node:assert/strict';
import fs from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import http from 'node:http';
import {createHash} from 'node:crypto';
import {once} from 'node:events';
import {downloadVerified, retryAfter} from './download.mjs';

const digest = bytes => createHash('sha256').update(bytes).digest('hex');
function fixture(t) {
  const root = fs.mkdtempSync(path.join(os.tmpdir(), 'loog-download-'));
  t.after(() => fs.rmSync(root, {recursive:true, force:true}));
  return {root, spec:{url:'https://example.invalid/pinned', destination:path.join(root, 'archive'), sha256:digest('complete')}};
}
const gate = () => {let release; const promise = new Promise(resolve => {release = resolve;}); return {promise, release};};

test('503 then 429 honors Retry-After and recovers without publishing partial data', async t => {
  const {root,spec} = fixture(t), pauses = []; let calls = 0;
  const file = await downloadVerified(spec, {wait:async ms => {
    pauses.push(ms); assert.deepEqual(fs.readdirSync(root), []);
  }, fetcher:async () => {
    calls++;
    return calls === 1 ? new Response('unavailable', {status:503}) : calls === 2
      ? new Response('limited', {status:429, headers:{'Retry-After':'3'}}) : new Response('complete');
  }});
  assert.equal(calls, 3); assert.deepEqual(pauses, [1000,3000]);
  assert.equal(fs.readFileSync(file, 'utf8'), 'complete');
  assert.deepEqual(fs.readdirSync(root), ['archive']);
  assert.equal(retryAfter(new Date(5000).toUTCString(), 1000), 4000);
});

test('an interrupted body is discarded and the next attempt starts with fresh bytes', async t => {
  const {root,spec} = fixture(t); let calls = 0;
  const file = await downloadVerified(spec, {wait:async () => {assert.deepEqual(fs.readdirSync(root), []);}, fetcher:async () => {
    if (++calls > 1) return new Response('complete');
    return new Response(new ReadableStream({pull(controller) {
      controller.enqueue(new TextEncoder().encode('partial'));
      controller.error(Object.assign(Error('connection reset'), {code:'ECONNRESET'}));
    }}));
  }});
  assert.equal(calls, 2); assert.equal(await fs.promises.readFile(file, 'utf8'), 'complete');
  assert.deepEqual(fs.readdirSync(root), ['archive']);
});

test('permanent HTTP and digest errors never retry or replace an existing destination', async t => {
  for (const response of [() => new Response('missing', {status:404}), () => new Response('bad digest')]) {
    const {root,spec} = fixture(t); fs.writeFileSync(spec.destination, 'previous'); let calls = 0;
    await assert.rejects(downloadVerified(spec, {wait:async () => assert.fail('must not retry'),
      fetcher:async () => {calls++; return response();}}), /404|SHA-256/);
    assert.equal(calls, 1); assert.equal(fs.readFileSync(spec.destination, 'utf8'), 'previous');
    assert.deepEqual(fs.readdirSync(root), ['archive']);
  }
});

test('transient failures exhaust the attempt limit and Retry-After cannot exceed the total budget', async t => {
  const {root,spec} = fixture(t); let calls = 0, waits = 0;
  await assert.rejects(downloadVerified(spec, {attempts:3, wait:async () => {waits++;}, fetcher:async () => {
    calls++; throw Object.assign(Error('reset'), {code:'ECONNRESET'});
  }}), /reset/);
  assert.equal(calls,3); assert.equal(waits,2); assert.deepEqual(fs.readdirSync(root), []);
  calls = 0;
  await assert.rejects(downloadVerified(spec, {timeoutMs:2000, wait:async () => assert.fail('budget exceeded'),
    fetcher:async () => {calls++; return new Response('', {status:429, headers:{'Retry-After':'60'}});}}), /429/);
  assert.equal(calls,1);
});

test('delayed headers and body publish only after the complete verified response', async t => {
  const {root,spec} = fixture(t), headers = gate(), body = gate(), entered = gate(), received = gate();
  const server = http.createServer(async (_request, response) => {
    entered.release(); await headers.promise;
    response.writeHead(200); response.write('com');
    await body.promise; response.end('plete');
  });
  t.after(() => {headers.release(); body.release(); server.closeAllConnections(); server.close();});
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  const pending = downloadVerified({...spec,url:'http://127.0.0.1:' + server.address().port}, {fetcher:async (...args) => {
    const response = await fetch(...args); received.release(); return response;
  }});
  await entered.promise; assert.equal(fs.existsSync(spec.destination), false);
  headers.release(); await received.promise; assert.equal(fs.existsSync(spec.destination), false);
  body.release(); await pending;
  assert.equal(fs.readFileSync(spec.destination, 'utf8'), 'complete');
  assert.deepEqual(fs.readdirSync(root), ['archive']);
});

test('a server that never responds is aborted within the download budget', async t => {
  const {root,spec} = fixture(t);
  const server = http.createServer(() => {});
  t.after(() => {server.closeAllConnections(); server.close();});
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  await assert.rejects(downloadVerified({...spec,url:'http://127.0.0.1:' + server.address().port},
    {timeoutMs:200, wait:async () => assert.fail('no retry outside budget')}), /timeout|aborted/i);
  assert.deepEqual(fs.readdirSync(root), []);
});

test('a response that stalls after headers cannot outlive the total download budget', async t => {
  const {root,spec} = fixture(t);
  const server = http.createServer((_request, response) => {response.writeHead(200); response.write('partial');});
  t.after(() => {server.closeAllConnections(); server.close();});
  server.listen(0, '127.0.0.1'); await once(server, 'listening');
  await assert.rejects(downloadVerified({...spec,url:'http://127.0.0.1:' + server.address().port},
    {timeoutMs:200, wait:async () => assert.fail('no retry outside budget')}), /timeout|aborted/i);
  assert.deepEqual(fs.readdirSync(root), []);
});
