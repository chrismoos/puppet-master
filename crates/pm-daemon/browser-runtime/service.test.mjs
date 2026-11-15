import test from 'node:test';
import assert from 'node:assert/strict';
import { spawn } from 'node:child_process';
import { createInterface } from 'node:readline';
import { once } from 'node:events';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { join } from 'node:path';
import { copyFileSync } from 'node:fs';
import { createHash } from 'node:crypto';

const runtime = process.env.PM_BROWSER_RUNTIME_DIR;
assert(runtime, 'Set PM_BROWSER_RUNTIME_DIR to an installed runtime.');
const require = createRequire(join(runtime, 'package.json'));
const { WebSocket } = require('ws');
for (const name of ['service.mjs', 'tools.json']) copyFileSync(new URL(name, import.meta.url), join(runtime, name));
const TEST_TIMEOUT_MS = 60000;
const SHORT_IDLE_MS = 1200;
function broker(extra = {}) {
  const child = spawn(process.execPath, [join(runtime, 'service.mjs')], { cwd: runtime, stdio: ['pipe', 'pipe', 'inherit'], env: { ...process.env, ...extra } });
  const pending = new Map();
  let sequence = 0;
  createInterface({ input: child.stdout }).on('line', line => {
    const response = JSON.parse(line);
    const waiter = pending.get(response.id);
    pending.delete(response.id);
    if (response.error) waiter.reject(Error(response.error)); else waiter.resolve(response.result);
  });
  child.on('exit', () => { for (const p of pending.values()) p.reject(Error('Runtime exited')); pending.clear(); });
  return {
    call(session_id, tool, args = {}) {
      return new Promise((resolve, reject) => {
        const id = ++sequence;
        pending.set(id, { resolve, reject });
        child.stdin.write(JSON.stringify({ id, session_id, tool, args }) + '\n');
      });
    },
    async close() { const exited = once(child, 'exit'); child.stdin.end(); await exited; },
  };
}
const resultText = result => result.content.filter(c => c.type === 'text').map(c => c.text).join('\n');
const evaluate = (b, id, fn, context = 'default') => b.call(id, 'browser_evaluate', { function: fn, browser_context: context });

test('managed browsing isolates logins, runs official tools, streams active tabs, and releases resources', { timeout: TEST_TIMEOUT_MS }, async t => {
  const b = broker();
  t.after(() => b.close());
  const app = createServer((_req, res) => {
    res.setHeader('Content-Type', 'text/html');
    res.end('<title>Managed browser test</title><h1>Settings</h1><input aria-label="Name"><button onclick="document.querySelector(\'h1\').textContent=document.querySelector(\'input\').value">Save</button>');
  });
  await new Promise(resolve => app.listen(0, '127.0.0.1', resolve));
  t.after(() => new Promise(resolve => app.close(resolve)));
  const url = 'http://127.0.0.1:' + app.address().port;
  await t.test('listing does not create a browser and server code tools are refused', async () => {
    assert.deepEqual(JSON.parse(resultText(await b.call(1, 'browser_contexts'))).contexts, []);
    await assert.rejects(b.call(1, 'browser_run_code_unsafe', { code: 'process.exit()' }), /not available/);
    await assert.rejects(b.call(1, 'browser_tabs', { action: 'new', isolatedContext: 'hidden' }), /browser_contexts/);
  });
  await t.test('ordinary MCP actions update the real page', async () => {
    assert(!((await b.call(1, 'browser_navigate', { url })).isError));
    await b.call(1, 'browser_type', { target: 'input', text: 'Verified settings' });
    await b.call(1, 'browser_click', { target: 'button' });
    assert.match(resultText(await evaluate(b, 1, '() => document.querySelector("h1").textContent')), /Verified settings/);
    await evaluate(b, 1, '() => { localStorage.setItem("account", "first"); document.cookie="account=first"; }');
  });
  await t.test('explicit contexts isolate storage and require a reason', async () => {
    await assert.rejects(b.call(1, 'browser_contexts', { action: 'create', browser_context: 'second' }), /reason/);
    await b.call(1, 'browser_contexts', { action: 'create', browser_context: 'second', reason: 'Test a separate logged-in user' });
    await b.call(1, 'browser_navigate', { url, browser_context: 'second' });
    assert.match(resultText(await evaluate(b, 1, '() => ({account:localStorage.getItem("account"),cookie:document.cookie})', 'second')), /"account": null/);
    assert.match(resultText(await evaluate(b, 1, '() => localStorage.getItem("account")')), /first/);
    await b.call(2, 'browser_navigate', { url });
    assert.match(resultText(await evaluate(b, 2, '() => localStorage.getItem("account")')), /null/);
    await b.call(2, '__close_session');
  });
  await t.test('a viewer receives JPEG frames and follows active tabs', async () => {
    const view = await b.call(1, '__view');
    const socket = new WebSocket(`ws://127.0.0.1:${view.port}/${view.path}`);
    t.after(() => socket.close());
    let frame;
    let status;
    socket.on('message', (data, binary) => { if (binary) frame = data; else status = JSON.parse(data); });
    await once(socket, 'open');
    await waitFor(() => frame?.[0] === 0xff && frame?.[1] === 0xd8);
    assert.equal(status.width, 1280);
    await b.call(1, 'browser_tabs', { action: 'new', url: url + '/second-tab' });
    await waitFor(() => status.url?.endsWith('/second-tab'));
    const resize = await b.call(1, 'browser_resize', { width: 900, height: 600 });
    assert(!resize.isError);
    await waitFor(() => status.width === 900 && status.height === 600);
    socket.close();
    await once(socket, 'close');
    const same = await b.call(1, '__view');
    assert.equal(same.path, view.path);
    await evaluate(b, 1, '() => { let color = 0; window.streamPulse = setInterval(() => { document.body.style.backgroundColor = `rgb(${color = (color + 31) % 255},50,90)`; }, 80); }');
    const cycles = 5;
    const freshFrames = 3;
    for (let cycle = 0; cycle < cycles; cycle++) {
      const viewer = new WebSocket(`ws://127.0.0.1:${view.port}/${view.path}`);
      const images = new Set();
      viewer.on('message', (data, binary) => { if (binary) images.add(createHash('sha1').update(data).digest('hex')); });
      t.after(() => viewer.close());
      await once(viewer, 'open');
      await waitFor(() => images.size >= freshFrames);
      viewer.close();
      await once(viewer, 'close');
    }
    await evaluate(b, 1, '() => clearInterval(window.streamPulse)');
    assert.match(resultText(await evaluate(b, 1, '() => location.pathname')), /second-tab/);
    await b.call(1, '__close_session');
    assert.deepEqual(JSON.parse(resultText(await b.call(1, 'browser_contexts'))).contexts, []);
    await assert.rejects(fetch(`http://127.0.0.1:${view.port}/${view.path}`).then(r => { if (!r.ok) throw Error('not found'); }), /not found/);
  });
});
async function waitFor(condition, timeout = 10000) {
  const start = Date.now();
  while (!condition()) {
    if (Date.now() - start > timeout) throw Error('Timed out waiting for browser state');
    await new Promise(resolve => setTimeout(resolve, 25));
  }
}
test('idle browsers expire and a later call starts with clean state', { timeout: TEST_TIMEOUT_MS }, async t => {
  const b = broker({ PM_BROWSER_IDLE_MS: String(SHORT_IDLE_MS) });
  t.after(() => b.close());
  await b.call(3, 'browser_navigate', { url: 'about:blank' });
  await new Promise(resolve => setTimeout(resolve, SHORT_IDLE_MS * 3));
  assert.deepEqual(JSON.parse(resultText(await b.call(3, 'browser_contexts'))).contexts, []);
  await b.call(3, 'browser_navigate', { url: 'about:blank' });
  assert.deepEqual(JSON.parse(resultText(await b.call(3, 'browser_contexts'))).contexts, ['default']);
});
