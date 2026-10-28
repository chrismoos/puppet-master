import { writeFile } from "node:fs/promises";
import { expect, test, type Page } from "./fixtures";
import { apiHeaders, expectTerminalRevealed, logIn } from "./support";

const PROBES = 150;
const GAP_MS = 40;
const PAINT_TIMEOUT_MS = 30_000;
const SOCKET_DIAGNOSTIC_LIMIT = 24;

interface ProbeState {
    sockets: Record<string, WebSocket>;
    order: string[];
    generation: Record<string, Uint8Array>;
    sent: Record<string, number>;
    received: Record<string, number>;
    output: Record<string, string>;
    painted: Record<string, number>;
    terminals: Record<string, string>;
    socketEvents: Record<string, {
      opened: number;
      messages: number;
      arrayBuffers: number;
      tags: Record<string, number>;
      lastDataType: string;
      closes: number[];
    }>;
}

type ProbeWindow = Window & { __pmProbe: ProbeState };

async function installProbe(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const probe = {
      sockets: {}, order: [], generation: {}, sent: {}, received: {}, output: {}, painted: {}, terminals: {}, socketEvents: {},
    } as ProbeWindow["__pmProbe"];
    (window as unknown as ProbeWindow).__pmProbe = probe;
    type PaintTerminal = {
      onRender(listener: () => void): { dispose(): void };
      renderSnapshot(): { rowData: Array<{ text: string }> } | null;
    };
    type PaintStage = {
      debugSnapshot(): Array<{ key: string; visible: boolean; socket: { phase: string } | null }>;
      layers: Map<string, { el: HTMLElement; term: PaintTerminal }>;
    };
    const stage = () => (window as unknown as { __pmStage?: PaintStage }).__pmStage;
    const pendingPaint = new Set<string>();
    const subscriptions = new Map<string, { terminal: PaintTerminal; dispose(): void }>();
    const visibleText = (key: string) => {
      const current = stage();
      const selected = current?.debugSnapshot().find((entry) => entry.visible);
      const layer = current?.layers.get(key);
      if (selected?.key !== key || selected.socket?.phase !== "online" || !layer
        || getComputedStyle(layer.el).visibility !== "visible"
        || layer.el.querySelector<HTMLElement>(".terminal-loading-curtain")?.hidden === false) return "";
      const canvas = layer.el.querySelector<HTMLCanvasElement>(".ghostty-canvas");
      if (!canvas || canvas.width <= 0 || canvas.height <= 0) return "";
      return (layer.term.renderSnapshot()?.rowData ?? []).map((row) => row.text).join("");
    };
    const rendered = (key: string) => {
      const text = visibleText(key);
      for (const id of Object.keys(probe.received)) {
        if (probe.terminals[id] !== key || probe.painted[id] !== undefined || pendingPaint.has(id)
          || !text.includes(`pmprobe-${id}`)) continue;
        pendingPaint.add(id);
        requestAnimationFrame(() => requestAnimationFrame(() => {
          pendingPaint.delete(id);
          if (visibleText(key).includes(`pmprobe-${id}`)) probe.painted[id] = performance.timeOrigin + performance.now();
        }));
      }
    };
    const subscribe = () => {
      const current = stage();
      for (const [key, subscription] of subscriptions) {
        if (current?.layers.get(key)?.term === subscription.terminal) continue;
        subscription.dispose();
        subscriptions.delete(key);
      }
      for (const [key, layer] of current?.layers ?? []) {
        if (subscriptions.has(key)) continue;
        const listener = layer.term.onRender(() => rendered(key));
        subscriptions.set(key, { terminal: layer.term, dispose: () => listener.dispose() });
      }
    };
    new MutationObserver(subscribe).observe(document, { subtree: true, childList: true });
    requestAnimationFrame(subscribe);
    const nativeWebSocket = window.WebSocket;
    window.WebSocket = new Proxy(nativeWebSocket, {
      construct(target, args) {
        const socket = Reflect.construct(target, args) as WebSocket;
        const raw = String(args[0]);
        if (!raw.includes("/ws/terminal/")) return socket;
        const url = new URL(raw, location.href).pathname;
        probe.sockets[url] = socket;
        probe.order.push(url);
        const events = probe.socketEvents[url] ??= {
          opened: 0, messages: 0, arrayBuffers: 0, tags: {}, lastDataType: "none", closes: [],
        };
        delete probe.generation[url];
        socket.addEventListener("open", () => events.opened += 1);
        socket.addEventListener("close", (event) => {
          events.closes.push(event.code);
          (probe as unknown as { closes: Record<string, number[]> }).closes ??= {};
          const closes = (probe as unknown as { closes: Record<string, number[]> }).closes;
          (closes[url] ??= []).push(event.code);
        });
        socket.addEventListener("message", (event) => {
          events.messages += 1;
          events.lastDataType = Object.prototype.toString.call(event.data);
          if (event.data instanceof ArrayBuffer) {
            events.arrayBuffers += 1;
            if (event.data.byteLength > 0) {
              const tag = String(new DataView(event.data).getUint8(0));
              events.tags[tag] = (events.tags[tag] ?? 0) + 1;
            }
          }
          if (!(event.data instanceof ArrayBuffer) || event.data.byteLength < 9) return;
          const bytes = new Uint8Array(event.data);
          if (bytes[0] !== 1 && bytes[0] !== 3) return;
          probe.generation[url] ??= bytes.slice(1, 9);
          if (bytes[0] !== 1) return;
          const text = new TextDecoder().decode(bytes.subarray(10));
          // A marker can straddle two frames, so match on the tail of the previous one too.
          const tail = (probe.output[url] ?? "").slice(-40);
          probe.output[url] = (tail + text).slice(-4000);
          const now = performance.timeOrigin + performance.now();
          for (const match of (tail + text).matchAll(/pmprobe-([a-z0-9]+)/g)) {
            probe.received[match[1]] ??= now;
          }
        });
        return socket;
      },
    });
  });
}

declare global {
  interface Window { __pmProbe: ProbeState }
}

function sendInput(page: Page, url: string, text: string, id?: string) {
  return page.evaluate(({ url, text, id }) => {
    const probe = (window as unknown as ProbeWindow).__pmProbe;
    const socket = probe.sockets[url];
    const generation = probe.generation[url];
    if (!socket || !generation || socket.readyState !== WebSocket.OPEN) throw new Error("socket not ready: " + url);
    const payload = new TextEncoder().encode(text);
    const frame = new Uint8Array(9 + payload.length);
    frame[0] = 2;
    frame.set(generation, 1);
    frame.set(payload, 9);
    if (id) {
      probe.sent[id] = performance.timeOrigin + performance.now();
      probe.terminals[id] = `t:${new URL(url, location.href).pathname.split("/").at(-1)!}`;
    }
    socket.send(frame);
  }, { url, text, id });
}

/** The newest terminal socket opened at or after position `after` that has seen a frame. */
async function latestTerminalSocket(page: Page, after: number): Promise<string> {
  const pick = (after: number) => {
    const probe = (window as unknown as ProbeWindow).__pmProbe;
    const ready = probe.order.slice(after).filter((url) => probe.generation[url] && probe.sockets[url].readyState === WebSocket.OPEN);
    return ready.length ? ready[ready.length - 1] : null;
  };
  try {
    await expect.poll(() => page.evaluate(pick, after), { timeout: 30_000 }).not.toBeNull();
  } catch (error) {
    await attachSocketDiagnostic(page, "terminal-socket-timeout", after);
    throw error;
  }
  return (await page.evaluate(pick, after))!;
}

async function attachSocketDiagnostic(page: Page, label: string, after: number): Promise<void> {
  const diagnostic = await page.evaluate(({ after, limit }) => {
    const probe = (window as unknown as ProbeWindow).__pmProbe;
    const path = (url: string) => new URL(url, location.href).pathname;
    const stage = (window as unknown as { __pmStage?: {
      debugSnapshot(): Array<{
        key: string; visible: boolean; cols: number; rows: number; socket: unknown;
        baseY: number; viewportY: number;
      }>;
    } }).__pmStage;
    return {
      route: location.hash, after, timeOrigin: performance.timeOrigin, at: performance.now(),
      orderCount: probe.order.length, order: probe.order.slice(-limit).map(path),
      candidates: probe.order.slice(after).slice(-limit).map(path),
      generation: Object.entries(probe.generation).slice(-limit)
        .map(([url, bytes]) => ({ path: path(url), bytes: [...bytes] })),
      sockets: Object.entries(probe.sockets).slice(-limit).map(([url, socket]) => ({
        path: path(url), readyState: socket.readyState, openConstant: WebSocket.OPEN,
        binaryType: socket.binaryType, observed: probe.socketEvents[url],
        hasGeneration: probe.generation[url] !== undefined,
      })),
      selectedTitle: document.querySelector(".sb-session.is-selected .sb-session-title")?.textContent,
      layers: stage?.debugSnapshot().map(({ key, visible, cols, rows, socket, baseY, viewportY }) => ({
        key, visible, cols, rows, socket, baseY, viewportY,
      })),
    };
  }, { after, limit: SOCKET_DIAGNOSTIC_LIMIT });
  console.log(`LATENCY_SOCKET_DIAGNOSTIC ${label} ${JSON.stringify(diagnostic)}`);
  await test.info().attach(label, { body: JSON.stringify(diagnostic, null, 2), contentType: "application/json" });
}

async function waitOpen(page: Page, url: string): Promise<void> {
  await expect.poll(() => page.evaluate((url) => {
    const probe = (window as unknown as ProbeWindow).__pmProbe;
    return Boolean(probe.generation[url] && probe.sockets[url]?.readyState === WebSocket.OPEN);
  }, url), { timeout: 30_000 }).toBe(true);
}

async function socketCount(page: Page): Promise<number> {
  return page.evaluate(() => (window as unknown as ProbeWindow).__pmProbe.order.length);
}

async function captureProbeResults(
  page: Page,
  url: string,
  ids: string[],
  label: string,
  out: string,
  coverage: "all" | "some" | "observed",
): Promise<number[]> {
  let paintFailure: unknown;
  if (coverage !== "observed") {
    try {
      await expect.poll(() => page.evaluate(({ ids, every }) => {
        const probe = (window as unknown as ProbeWindow).__pmProbe;
        const received = ids.filter((id) => probe.received[id] !== undefined);
        const painted = received.filter((id) => probe.painted[id] !== undefined);
        return received.length > 0 && (every ? painted.length === received.length : painted.length > 0);
      }, { ids, every: coverage === "all" }), {
        message: `${label} Canvas paint coverage for received markers`,
        timeout: PAINT_TIMEOUT_MS,
      }).toBe(true);
    } catch (error: unknown) { paintFailure = error; }
  }
  const stamps = await page.evaluate((ids) => {
    const probe = (window as unknown as ProbeWindow).__pmProbe;
    return ids.map((id) => ({ id, sent: probe.sent[id], received: probe.received[id] ?? null, painted: probe.painted[id] ?? null }));
  }, ids);
  const rtts = stamps.filter((stamp) => stamp.received !== null).map((stamp) => stamp.received! - stamp.sent);
  const paintLatencies = stamps.filter((stamp) => stamp.painted !== null).map((stamp) => stamp.painted! - stamp.sent);
  const received = stamps.filter((stamp) => stamp.received !== null).length;
  const unpainted = stamps.filter((stamp) => stamp.received !== null && stamp.painted === null).map((stamp) => stamp.id);
  const churn = await page.evaluate((url) => {
    const probe = window.__pmProbe as unknown as { order: string[]; closes?: Record<string, number[]> };
    return { sockets: probe.order.filter((value) => value === url).length, closes: probe.closes?.[url] ?? [] };
  }, url);
  await writeFile(`${out}/${label}.json`, JSON.stringify({
    label, ids, rtts, paintLatencies, stamps, lost: ids.length - rtts.length,
    paintCoverage: { requirement: coverage, received, painted: paintLatencies.length, unpainted }, churn,
  }));
  if (paintFailure) throw paintFailure;
  return rtts;
}

async function runProbes(page: Page, url: string, label: string, out: string, coverage: "all" | "some" = "all"): Promise<number[]> {
  await page.bringToFront();
  const ids: string[] = [];
  for (let i = 0; i < PROBES; i++) {
    const id = `${label}${i.toString(36)}x${Math.random().toString(36).slice(2, 6)}`;
    ids.push(id);
    await sendInput(page, url, `\x1b_pmprobe-${id}\x1b\\.${i % 20 === 19 ? "\n" : ""}`, id);
    // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
    await page.waitForTimeout(GAP_MS);
  }
  // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
  await page.waitForTimeout(500);
  return captureProbeResults(page, url, ids, label, out, coverage);
}

async function runProbesTolerant(page: Page, url: string, label: string, out: string): Promise<number[]> {
  await page.bringToFront();
  const ids: string[] = [];
  for (let i = 0; i < PROBES; i++) {
    const id = `${label}${i.toString(36)}x${Math.random().toString(36).slice(2, 6)}`;
    ids.push(id);
    try {
      await sendInput(page, url, `\x1b_pmprobe-${id}\x1b\\.${i % 20 === 19 ? "\n" : ""}`, id);
    } catch {
      await page.evaluate((id) => { (window as unknown as ProbeWindow).__pmProbe.sent[id] = performance.timeOrigin + performance.now(); }, id);
    }
    // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
    await page.waitForTimeout(GAP_MS);
  }
  // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
  await page.waitForTimeout(500);
  return captureProbeResults(page, url, ids, label, out, "observed");
}

async function openShell(page: Page): Promise<string> {
  const before = await socketCount(page);
  await page.locator(".terminal-tabs button", { hasText: "+ Shell" }).click();
  const url = await latestTerminalSocket(page, before);
  const terminalId = new URL(url, page.url()).pathname.split("/").at(-1)!;
  await expectTerminalRevealed(page, `t:${terminalId}`);
  // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
  await page.waitForTimeout(1500);
  await sendInput(page, url, "cat\n");
  // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
  await page.waitForTimeout(500);
  return url;
}

test("pty latency ladder", async ({ page, isolatedDaemon, context }, testInfo) => {
  const out = testInfo.outputPath();
  test.setTimeout(600_000);
  await installProbe(page);
  await logIn(page);
  const base = process.env.PM_E2E_BASE_URL!;
  const headers = { ...(await apiHeaders(page)), "content-type": "application/json" };

  // A real remote worker, enrolled the way an operator does it.
  await page.goto(`${base}/#/settings/workers`);
  await page.getByRole("button", { name: "Add worker" }).click();
  const form = page.getByRole("dialog", { name: "Add a worker" });
  await form.getByRole("group", { name: "Location" }).getByRole("button", { name: "Remote" }).click();
  await form.getByLabel("Name", { exact: true }).fill("lat-box");
  await form.getByRole("button", { name: "Generate command" }).click();
  const minted = await form.locator(".enroll-result .ui-cmd code").textContent();
  const token = /--token (\S+)$/.exec(minted!)![1];
  await form.getByRole("button", { name: "Done" }).click();
  await isolatedDaemon.startRemoteWorker(token);
  await expect(page.locator(".worker-row", { hasText: "lat-box" }).locator(".worker-status")).toHaveText("Online");
  const remoteId = isolatedDaemon.workerId("lat-box");
  expect(remoteId).not.toBeNull();
  const allowed = [0, remoteId!];
  const bucketSet = await page.request.post(`${base}/api/buckets/1/worker`, { headers, data: JSON.stringify({ worker_id: 0, allowed_worker_ids: allowed }) });
  expect(bucketSet.ok(), await bucketSet.text()).toBe(true);
  const projectSet = await page.request.post(`${base}/api/projects/1/worker`, { headers, data: JSON.stringify({ worker_id: remoteId, allowed_worker_ids: allowed }) });
  expect(projectSet.ok(), await projectSet.text()).toBe(true);

  // A session on the remote worker, then a shell in it running cat.
  await page.goto(`${base}/#/`);
  await page.locator(".sb-bucket-row").first().getByTitle(/actions for /).click();
  await page.getByRole("menuitem", { name: "new session…" }).click();
  const workerPick = page.getByRole("combobox", { name: "worker", exact: true });
  await expect(workerPick).toBeVisible();
  await workerPick.selectOption(String(remoteId));
  await page.getByRole("combobox", { name: /^agent/ }).selectOption("codex");
  await page.getByLabel("title (optional)").fill("latency-remote");
  const before = await socketCount(page);
  await attachSocketDiagnostic(page, "before-remote-spawn", before);
  await page.getByRole("button", { name: "spawn", exact: true }).click();
  await expect(page.getByRole("button", { name: "spawn", exact: true })).toBeHidden();
  await expect(page).toHaveURL(/#\/session\/\d+$/);
  await expect(page.locator(".sb-session.is-selected .sb-session-title")).toHaveText("latency-remote");
  const remoteSessionId = page.url().match(/#\/session\/(\d+)$/)![1];
  await expectTerminalRevealed(page, `s:${remoteSessionId}`);
  await latestTerminalSocket(page, before);
  // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
  await page.waitForTimeout(2000);
  const remoteShell = await openShell(page);

  const results: Record<string, number[]> = {};
  results.remote_idle = await runProbes(page, remoteShell, "ri", out);

  // Same terminal with steady output like an agent's: a line every few ms.
  await sendInput(page, remoteShell, "\x03");
  // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
  await page.waitForTimeout(300);
  await sendInput(page, remoteShell, "timeout 20 sh -c 'while true; do date +%T.%N; sleep 0.005; done' & cat\n");
  // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
  await page.waitForTimeout(2000);
  results.remote_steady_same = await runProbes(page, remoteShell, "rs", out, "some");
  // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
  await page.waitForTimeout(14_000);
  await sendInput(page, remoteShell, "\x03");
  // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
  await page.waitForTimeout(300);
  await sendInput(page, remoteShell, "clear; cat\n");
  // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
  await page.waitForTimeout(1500);
  // Same terminal while it floods flat out for 15 s: yes in the background, cat still echoing.
  await sendInput(page, remoteShell, "\x03");
  // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
  await page.waitForTimeout(300);
  await sendInput(page, remoteShell, "timeout 15 yes & cat\n");
  // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
  await page.waitForTimeout(1500);
  results.remote_flood_same = await runProbesTolerant(page, remoteShell, "rf", out);
  // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
  await page.waitForTimeout(16_000);
  await waitOpen(page, remoteShell);
  // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
  await page.waitForTimeout(2000);
  await sendInput(page, remoteShell, "\x03");
  // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
  await page.waitForTimeout(300);
  await sendInput(page, remoteShell, "clear; cat\n");
  // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
  await page.waitForTimeout(1500);

  // Three viewers on the same terminal.
  const viewers = [await context.newPage(), await context.newPage()];
  for (const viewer of viewers) {
    await viewer.goto(page.url());
    // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
    await viewer.waitForTimeout(3000);
  }
  results.remote_three_viewers = await runProbes(page, remoteShell, "rv", out);
  for (const viewer of viewers) await viewer.close();
  // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
  await page.waitForTimeout(1000);
  results.remote_idle_again = await runProbes(page, remoteShell, "rj", out);

  // The local worker's shell for comparison.
  await page.goto(`${base}/#/`);
  await page.locator(".sb-session").filter({
    has: page.locator(".sb-session-title", { hasText: /^browser-e2e$/ }),
  }).click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);
  const localSessionId = page.url().match(/#\/session\/(\d+)$/)?.[1];
  if (!localSessionId) throw new Error("local session route was not selected");
  await expectTerminalRevealed(page, `s:${localSessionId}`);
  // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
  await page.waitForTimeout(2000);
  const localShell = await openShell(page);
  results.local_idle = await runProbes(page, localShell, "li", out);

  await writeFile(`${out}/summary.json`, JSON.stringify(results));
});
