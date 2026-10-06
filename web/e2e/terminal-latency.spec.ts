import { writeFile } from "node:fs/promises";
import { expect, test, type Page } from "./fixtures";
import { apiHeaders, logIn } from "./support";

const PROBES = 150;
const GAP_MS = 40;

type ProbeWindow = Window & {
  __pmProbe: {
    sockets: Record<string, WebSocket>;
    order: string[];
    generation: Record<string, Uint8Array>;
    sent: Record<string, number>;
    received: Record<string, number>;
    output: Record<string, string>;
  };
};

async function installProbe(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const probe = { sockets: {}, order: [], generation: {}, sent: {}, received: {}, output: {} } as ProbeWindow["__pmProbe"];
    (window as unknown as ProbeWindow).__pmProbe = probe;
    const painted: Record<string, number> = {};
    (probe as unknown as { painted: Record<string, number> }).painted = painted;
    const markPaint = () => {
      const now = performance.timeOrigin + performance.now();
      for (const id of Object.keys(probe.received)) {
        if (painted[id] === undefined) painted[id] = now;
      }
    };
    for (const proto of [WebGL2RenderingContext.prototype, WebGLRenderingContext.prototype]) {
      for (const name of ["drawElements", "drawArrays", "drawElementsInstanced", "drawArraysInstanced"] as const) {
        const original = (proto as unknown as Record<string, (...args: unknown[]) => unknown>)[name];
        if (!original) continue;
        (proto as unknown as Record<string, (...args: unknown[]) => unknown>)[name] = function (this: unknown, ...args: unknown[]) {
          const result = original.apply(this, args);
          markPaint();
          return result;
        };
      }
    }
    const fillText = CanvasRenderingContext2D.prototype.fillText;
    CanvasRenderingContext2D.prototype.fillText = function (this: CanvasRenderingContext2D, ...args: Parameters<typeof fillText>) {
      const result = fillText.apply(this, args);
      markPaint();
      return result;
    };
    const nativeWebSocket = window.WebSocket;
    window.WebSocket = new Proxy(nativeWebSocket, {
      construct(target, args) {
        const socket = Reflect.construct(target, args) as WebSocket;
        const raw = String(args[0]);
        if (!raw.includes("/ws/terminal/")) return socket;
        const url = new URL(raw, location.href).pathname;
        probe.sockets[url] = socket;
        probe.order.push(url);
        delete probe.generation[url];
        socket.addEventListener("close", (event) => {
          (probe as unknown as { closes: Record<string, number[]> }).closes ??= {};
          const closes = (probe as unknown as { closes: Record<string, number[]> }).closes;
          (closes[url] ??= []).push(event.code);
        });
        socket.addEventListener("message", (event) => {
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
  interface Window { __pmProbe: ProbeWindow["__pmProbe"] }
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
    if (id) probe.sent[id] = performance.timeOrigin + performance.now();
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
  await expect.poll(() => page.evaluate(pick, after), { timeout: 30_000 }).not.toBeNull();
  return (await page.evaluate(pick, after))!;
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

async function runProbes(page: Page, url: string, label: string, out: string): Promise<number[]> {
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
  const stamps = await page.evaluate((ids) => {
    const probe = (window as unknown as ProbeWindow).__pmProbe;
    const painted = (probe as unknown as { painted: Record<string, number> }).painted;
    return ids.map((id) => ({ id, sent: probe.sent[id], received: probe.received[id] ?? null, painted: painted[id] ?? null }));
  }, ids);
  const times = stamps.map((s) => s.received !== null ? s.received - s.sent : null);
  const rtts = times.filter((t): t is number => t !== null);
  const churn = await page.evaluate((url) => {
    const probe = window.__pmProbe as unknown as { order: string[]; closes?: Record<string, number[]> };
    return { sockets: probe.order.filter((u) => u === url).length, closes: probe.closes?.[url] ?? [] };
  }, url);
  await writeFile(`${out}/${label}.json`, JSON.stringify({ label, ids, rtts, stamps, lost: times.length - rtts.length, churn }));
  return rtts;
}

async function runProbesTolerant(page: Page, url: string, label: string, out: string): Promise<number[]> {
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
  const stamps = await page.evaluate((ids) => {
    const probe = (window as unknown as ProbeWindow).__pmProbe;
    const painted = (probe as unknown as { painted: Record<string, number> }).painted;
    return ids.map((id) => ({ id, sent: probe.sent[id], received: probe.received[id] ?? null, painted: painted[id] ?? null }));
  }, ids);
  const times = stamps.map((s) => s.received !== null ? s.received - s.sent : null);
  const rtts = times.filter((t): t is number => t !== null);
  const churn = await page.evaluate((url) => {
    const probe = window.__pmProbe as unknown as { order: string[]; closes?: Record<string, number[]> };
    return { sockets: probe.order.filter((u) => u === url).length, closes: probe.closes?.[url] ?? [] };
  }, url);
  await writeFile(`${out}/${label}.json`, JSON.stringify({ label, ids, rtts, stamps, lost: times.length - rtts.length, churn }));
  return rtts;
}

async function openShell(page: Page): Promise<string> {
  const before = await socketCount(page);
  await page.locator(".terminal-tabs button", { hasText: "+ Shell" }).click();
  const url = await latestTerminalSocket(page, before);
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
  await page.getByRole("button", { name: "spawn", exact: true }).click();
  await expect(page.getByRole("button", { name: "spawn", exact: true })).toBeHidden();
  const before = await socketCount(page);
  await page.locator(".sb-session-row").filter({ hasNotText: "browser-e2e" }).first().click();
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
  results.remote_steady_same = await runProbes(page, remoteShell, "rs", out);
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
  await page.locator(".sb-session-row", { hasText: "browser-e2e" }).first().click();
  // e2e-real-time-wait: this spec measures latency over real time, so probe spacing and settle periods are the contract
  await page.waitForTimeout(2000);
  const localShell = await openShell(page);
  results.local_idle = await runProbes(page, localShell, "li", out);

  await writeFile(`${out}/summary.json`, JSON.stringify(results));
});
