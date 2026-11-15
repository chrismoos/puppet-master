import { createConnection } from '@playwright/mcp';
import { Client } from '@modelcontextprotocol/sdk/client/index.js';
import { InMemoryTransport } from '@modelcontextprotocol/sdk/inMemory.js';
import { chromium } from 'playwright';
import { WebSocketServer, WebSocket } from 'ws';
import { createServer } from 'node:http';
import { randomBytes } from 'node:crypto';
import { createInterface } from 'node:readline';
import { readFileSync } from 'node:fs';

const MAX_CONTEXTS = 4;
const IDLE_MS = Number(process.env.PM_BROWSER_IDLE_MS) || 15 * 60 * 1000;
const SWEEP_MS = Math.min(IDLE_MS, 30 * 1000);
const FRAME_INTERVAL_MS = 1000 / 15;
const MAX_BUFFER_BYTES = 512 * 1024;
const MIN_VIEWPORT = { width: 320, height: 240 };
const MAX_VIEWPORT = { width: 1920, height: 1200 };
const MAX_VIEWER_MESSAGE_BYTES = 1024;
const HTTP_NOT_FOUND = 404;
const WS_NORMAL_CLOSE = 1000;
const VIEWPORT = { width: 1280, height: 800 };
const JPEG_QUALITY = 70;
const TOOL_TIMEOUT_MS = 60 * 1000;
const allowedTools = new Set(JSON.parse(readFileSync(new URL('tools.json', import.meta.url))).map(t => t.name));
const sessions = new Map();
const queues = new Map();
const views = new Map();
const http = createServer((_req, res) => res.writeHead(HTTP_NOT_FOUND).end());
const sockets = new WebSocketServer({ noServer: true, maxPayload: MAX_VIEWER_MESSAGE_BYTES });

function validContext(name) {
  if (!/^[a-z][a-z0-9-]{0,39}$/.test(name)) throw Error('Context names must use lowercase letters, digits, and hyphens, starting with a letter.');
  return name;
}
function textResult(value) {
  return { content: [{ type: 'text', text: typeof value === 'string' ? value : JSON.stringify(value) }] };
}
async function owner(id) {
  let entry = sessions.get(id);
  if (!entry) {
    const browser = await chromium.launch({ headless: true });
    entry = { browser, contexts: new Map(), touched: Date.now() };
    sessions.set(id, entry);
    browser.on('disconnected', () => { if (sessions.get(id) === entry) void closeSession(id); });
  }
  entry.touched = Date.now();
  return entry;
}
async function makeContext(entry, name) {
  if (entry.contexts.size >= MAX_CONTEXTS) throw Error(`At most ${MAX_CONTEXTS} browser contexts per session. Reuse or close an existing context.`);
  const context = await entry.browser.newContext({ viewport: VIEWPORT, deviceScaleFactor: 1 });
  const server = await createConnection({ timeouts: { action: 10000, navigation: 30000 }, outputMode: 'stdout' }, () => Promise.resolve(context));
  const client = new Client({ name: 'puppet-master-browser', version: '1.0.0' });
  const [clientTransport, serverTransport] = InMemoryTransport.createLinkedPair();
  await server.connect(serverTransport);
  await client.connect(clientTransport);
  const state = { context, client, server, name, viewers: new Set(), page: null, cdp: null, capturing: null, frame: null, action: '', path: randomBytes(24).toString('hex') };
  views.set(state.path, state);
  entry.contexts.set(name, state);
  return state;
}
async function stateFor(id, name) {
  const entry = await owner(id);
  let state = entry.contexts.get(name);
  if (!state && name === 'default') state = await makeContext(entry, name);
  if (!state) throw Error(`No browser context named ${name}. Create one explicitly with browser_contexts and explain why a separate login is needed.`);
  return state;
}
async function currentPage(state) {
  const result = await state.client.callTool({ name: 'browser_tabs', arguments: { action: 'list' } });
  const text = result.content.filter(c => c.type === 'text').map(c => c.text).join('\n');
  const index = text.match(/^- (\d+):[^\n]*\(current\)/m)?.[1];
  const pages = state.context.pages();
  return pages[index === undefined ? pages.length - 1 : Number(index)] || null;
}
function sendStatus(state, status = 'live') {
  const page = state.page;
  const viewport = page?.viewportSize() || VIEWPORT;
  const message = JSON.stringify({ state: status, url: page?.url() || '', action: state.action, ...viewport });
  for (const socket of state.viewers) if (socket.readyState === WebSocket.OPEN && socket.bufferedAmount < MAX_BUFFER_BYTES) socket.send(message);
}
async function stopCapture(state) {
  clearTimeout(state.frameTimer);
  state.pendingFrame = null;
  const cdp = state.cdp;
  state.cdp = null;
  if (cdp) await cdp.detach().catch(() => {});
}
async function updateCapture(state) {
  const task = (state.capturing || Promise.resolve()).catch(() => {}).then(async () => {
    if (state.closed) return;
    const page = await currentPage(state);
    if (state.page !== page || !state.viewers.size) {
      await stopCapture(state);
      state.page = page;
      state.frame = null;
    }
    sendStatus(state, page ? 'live' : 'empty');
    if (!state.viewers.size || !page || state.cdp) return;
    const cdp = await state.context.newCDPSession(page);
    state.cdp = cdp;
    let lastFrame = 0;
    function flushFrame() {
      state.frameTimer = null;
      if (!state.pendingFrame || state.cdp !== cdp) return;
      lastFrame = Date.now();
      const frame = Buffer.from(state.pendingFrame, 'base64');
      state.pendingFrame = null;
      state.frame = frame;
      for (const socket of state.viewers) if (socket.readyState === WebSocket.OPEN && socket.bufferedAmount < MAX_BUFFER_BYTES) socket.send(frame);
    }
    cdp.on('Page.screencastFrame', event => {
      void cdp.send('Page.screencastFrameAck', { sessionId: event.sessionId }).catch(() => {});
      state.pendingFrame = event.data;
      if (!state.frameTimer) state.frameTimer = setTimeout(flushFrame, Math.max(0, FRAME_INTERVAL_MS - (Date.now() - lastFrame)));
    });
    await cdp.send('Page.startScreencast', { format: 'jpeg', quality: JPEG_QUALITY, maxWidth: MAX_VIEWPORT.width, maxHeight: MAX_VIEWPORT.height });
  });
  state.capturing = task;
  try { await task; } finally { if (state.capturing === task) state.capturing = null; }
}
async function closeContext(state) {
  state.closed = true;
  views.delete(state.path);
  for (const socket of state.viewers) socket.close(WS_NORMAL_CLOSE, 'Browser context closed');
  state.viewers.clear();
  await stopCapture(state);
  await state.client.close().catch(() => {});
  await state.context.close().catch(() => {});
}
async function closeSession(id) {
  const entry = sessions.get(id);
  if (!entry) return;
  sessions.delete(id);
  for (const state of entry.contexts.values()) await closeContext(state);
  await entry.browser.close().catch(() => {});
}
export async function dispatch({ session_id: id, tool, args = {} }) {
  if (tool === '__close_session') { await closeSession(id); return {}; }
  const name = validContext(args.browser_context || 'default');
  if (tool === 'browser_contexts') {
    const action = args.action || 'list';
    if (action === 'list') return textResult({ contexts: [...(sessions.get(id)?.contexts.keys() || [])], default_context: 'default', limit: MAX_CONTEXTS });
    if (action === 'close') {
      const entry = sessions.get(id);
      const state = entry?.contexts.get(name);
      if (state) { entry.contexts.delete(name); await closeContext(state); }
      if (entry && !entry.contexts.size) await closeSession(id);
      return textResult({ closed: name });
    }
    if (action !== 'create') throw Error('Unknown browser_contexts action.');
    if (!args.reason?.trim()) throw Error('Give a reason for creating another context. Reuse default unless separate cookies or logins are required.');
    const entry = await owner(id);
    if (!entry.contexts.has('default')) await makeContext(entry, 'default');
    if (!entry.contexts.has(name)) await makeContext(entry, name);
    return textResult({ browser_context: name, contexts: [...entry.contexts.keys()] });
  }
  if (args.isolatedContext !== undefined) throw Error('Use browser_contexts with a reason to create isolated contexts.');
  if (tool !== '__view' && !allowedTools.has(tool)) throw Error('That browser tool is not available.');
  if (tool === 'browser_resize' && (!Number.isInteger(args.width) || !Number.isInteger(args.height) || args.width < MIN_VIEWPORT.width || args.width > MAX_VIEWPORT.width || args.height < MIN_VIEWPORT.height || args.height > MAX_VIEWPORT.height)) throw Error('Viewport must be between 320 by 240 and 1920 by 1200.');
  const state = await stateFor(id, name);
  if (tool === '__view') {
    if (!state.context.pages().length) await state.context.newPage();
    return { port: http.address().port, path: state.path, context: name };
  }
  const { browser_context: _name, ...arguments_ } = args;
  state.action = tool.replace(/^browser_/, '').replaceAll('_', ' ');
  sendStatus(state);
  const result = await state.client.callTool({ name: tool, arguments: arguments_ }, undefined, { timeout: TOOL_TIMEOUT_MS });
  state.action = result.isError ? `${state.action} failed` : `${state.action} complete`;
  await updateCapture(state).catch(() => sendStatus(state, 'disconnected'));
  return result;
}
http.on('upgrade', (req, socket, head) => {
  const state = views.get(req.url?.slice(1));
  if (!state) { socket.destroy(); return; }
  sockets.handleUpgrade(req, socket, head, ws => {
    state.viewers.add(ws);
    ws.on('error', () => {});
    ws.on('close', () => { state.viewers.delete(ws); if (!state.viewers.size) void updateCapture(state).catch(() => {}); });
    if (state.frame) ws.send(state.frame);
    void updateCapture(state).catch(() => { sendStatus(state, 'disconnected'); });
  });
});
const sweep = setInterval(() => {
  for (const [id, entry] of sessions) {
    if (Date.now() - entry.touched > IDLE_MS && !queues.has(id)) void closeSession(id);
  }
}, SWEEP_MS);
sweep.unref();
await new Promise(resolve => http.listen(0, '127.0.0.1', resolve));
const lines = createInterface({ input: process.stdin });
function respond(id, result, error) { process.stdout.write(JSON.stringify({ id, result, error }) + '\n'); }
lines.on('line', line => {
  let request;
  try { request = JSON.parse(line); } catch { return; }
  const previous = queues.get(request.session_id) || Promise.resolve();
  const next = previous.catch(() => {}).then(() => dispatch(request));
  queues.set(request.session_id, next);
  void next.then(result => respond(request.id, result), error => respond(request.id, null, error.message)).finally(() => {
    if (queues.get(request.session_id) === next) queues.delete(request.session_id);
  });
});
async function shutdown() {
  clearInterval(sweep);
  for (const id of [...sessions.keys()]) await closeSession(id);
  sockets.close();
  http.close();
  process.exit(0);
}
lines.on('close', () => void shutdown());
process.on('SIGTERM', () => void shutdown());
process.on('SIGINT', () => void shutdown());
