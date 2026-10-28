import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";
const SHELL_COUNT = 24;
const CLOSE_COUNT = 8;
const SWITCH_COUNT = 100;
const OUTPUT_BURST_BYTES = 8 * 1024 * 1024;
const FLOOD_SWITCH_AFTER_BYTES = 8 * 1024;
const MEASURE_TIMEOUT_MS = 10_000;
const SHELL_CREATE_P99_BUDGET_MS = 250;
const AGENT_RETURN_BUDGET_MS = 100;
const WARM_SWITCH_P99_BUDGET_MS = 100;
const CLOSE_P99_BUDGET_MS = 300;
const FLOOD_OTHER_TERMINAL_BUDGET_MS = 500;
const FLOODED_AGENT_RETURN_BUDGET_MS = 750;
const LONG_TASK_BUDGET_MS = 200;

interface StreamMetrics {
  created: number;
  opened: number;
  closed: number;
  closeCodes: number[];
  bytes: number;
  replayEnds: number[];
  tail: string;
}

interface BrowserMetrics {
  streams: Record<string, StreamMetrics>;
  controlOpened: number;
  controlClosed: number;
  longTasks: Array<{ startTime: number; duration: number }>;
  floodAction?: {
    url: string;
    triggerBytes: number;
    knownUrls: string[];
    started?: number;
  };
}

interface PaintProbe {
  visibleKey(): string | null;
  reveal(key: string, timeout: number): Promise<void>;
  marker(key: string, text: string, timeout: number): Promise<void>;
}

type MetricsWindow = Window & { __pmMetrics: BrowserMetrics; __pmPaint: PaintProbe };

interface PaintStage {
  debugSnapshot(): Array<{ key: string; visible: boolean; socket: { phase: string } | null }>;
  layers: Map<string, { el: HTMLElement; term: {
    renderSnapshot(): { rowData: Array<{ text: string }> } | null;
  } }>;
}

function percentile(values: number[], fraction: number): number {
  const sorted = [...values].sort((first, second) => first - second);
  return sorted[Math.min(sorted.length - 1, Math.ceil(sorted.length * fraction) - 1)];
}

function summary(values: number[]) {
  const round = (value: number) => Math.round(value * 10) / 10;
  return {
    count: values.length,
    p50: round(percentile(values, 0.5)),
    p90: round(percentile(values, 0.9)),
    p99: round(percentile(values, 0.99)),
    max: round(Math.max(...values)),
  };
}

async function installMetrics(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const metrics: BrowserMetrics = {
      streams: {},
      controlOpened: 0,
      controlClosed: 0,
      longTasks: [],
    };
    (window as unknown as MetricsWindow).__pmMetrics = metrics;
    const stage = () => (window as unknown as { __pmStage?: PaintStage }).__pmStage;
    const visibleKey = () => stage()?.debugSnapshot().find((entry) => entry.visible)?.key ?? null;
    const visibleRoot = (key: string) => {
      const current = stage();
      const selected = current?.debugSnapshot().find((entry) => entry.visible);
      const layer = current?.layers.get(key)?.el;
      if (selected?.key !== key || selected.socket?.phase !== "online" || !layer
        || getComputedStyle(layer).visibility !== "visible"
        || layer.querySelector<HTMLElement>(".terminal-loading-curtain")?.hidden === false) return null;
      return layer.querySelector<HTMLCanvasElement>(".ghostty-canvas");
    };
    const frame = () => new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    const waitForPaint = async (key: string, text: string | null, timeout: number) => {
      const deadline = performance.now() + timeout;
      const ready = () => {
        const root = visibleRoot(key);
        if (!root || root.clientWidth <= 0 || root.clientHeight <= 0) return false;
        const snapshot = stage()?.layers.get(key)?.term.renderSnapshot();
        return root.width > 0 && root.height > 0 && snapshot !== null && snapshot !== undefined
          && (text === null || snapshot.rowData.some((row) => row.text.includes(text)));
      };
      while (performance.now() < deadline) {
        if (ready()) {
          await frame();
          if (ready()) return;
        } else await frame();
      }
      throw new Error(`terminal ${key} did not paint ${text ?? "its revealed grid"}`);
    };
    (window as unknown as MetricsWindow).__pmPaint = {
      visibleKey,
      reveal: (key, timeout) => waitForPaint(key, null, timeout),
      marker: (key, text, timeout) => waitForPaint(key, text, timeout),
    };
    const nativeWebSocket = window.WebSocket;
    const observedWebSocket = new Proxy(nativeWebSocket, {
      construct(target, args) {
        const socket = Reflect.construct(target, args) as WebSocket;
        const url = String(args[0]);
        if (url.includes("/ws/terminal/")) {
          const stream = metrics.streams[url] ??= {
            created: 0,
            opened: 0,
            closed: 0,
            closeCodes: [],
            bytes: 0,
            replayEnds: [],
            tail: "",
          };
          stream.created += 1;
          socket.addEventListener("open", () => stream.opened += 1);
          socket.addEventListener("close", (event) => {
            stream.closed += 1;
            stream.closeCodes.push(event.code);
          });
          socket.addEventListener("message", (event) => {
            if (!(event.data instanceof ArrayBuffer) || event.data.byteLength < 10) return;
            const view = new DataView(event.data);
            if (view.getUint8(0) !== 1) return;
            const byteLength = event.data.byteLength - 10;
            stream.bytes += byteLength;
            if ((view.getUint8(9) & 4) !== 0) stream.replayEnds.push(performance.now());
            if (byteLength <= 4_096) {
              const text = new TextDecoder().decode(new Uint8Array(event.data, 10));
              stream.tail = `${stream.tail}${text}`.slice(-8_192);
            }
            const action = metrics.floodAction;
            if (action?.url === url && action.started === undefined && stream.bytes >= action.triggerBytes) {
              const button = [...document.querySelectorAll<HTMLButtonElement>(".terminal-tabs button")]
                .find((candidate) => candidate.textContent?.trim() === "+ Shell");
              if (button) {
                action.started = performance.now();
                button.click();
              }
            }
          });
        } else if (new URL(url, location.href).pathname === "/ws") {
          socket.addEventListener("open", () => metrics.controlOpened += 1);
          socket.addEventListener("close", () => metrics.controlClosed += 1);
        }
        return socket;
      },
    });
    window.WebSocket = observedWebSocket as typeof WebSocket;
    try {
      new PerformanceObserver((entries) => {
        for (const entry of entries.getEntries()) {
          metrics.longTasks.push({ startTime: entry.startTime, duration: entry.duration });
        }
      }).observe({ type: "longtask", buffered: true });
    } catch {}
  });
}

async function openSession(page: Page): Promise<string[]> {
  const errors: string[] = [];
  page.on("pageerror", (error) => errors.push(String(error)));
  await installMetrics(page);
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await page.locator(".terminal-tabs").waitFor();
  await page.waitForFunction(() => {
    const metrics = (window as unknown as MetricsWindow).__pmMetrics;
    return Object.values(metrics.streams).some((stream) => stream.opened > 0 && stream.closed === 0 && stream.replayEnds.length > 0);
  });
  await page.evaluate(async ({ timeout }) => {
    const paint = (window as unknown as MetricsWindow).__pmPaint;
    const key = paint.visibleKey();
    if (!key) throw new Error("missing selected agent layer");
    await paint.reveal(key, timeout);
  }, { timeout: MEASURE_TIMEOUT_MS });
  return errors;
}

async function resetLongTasks(page: Page): Promise<void> {
  await page.evaluate(() => {
    (window as unknown as MetricsWindow).__pmMetrics.longTasks = [];
  });
}

async function createShellToReplay(page: Page): Promise<number> {
  return page.evaluate(async ({ timeout }) => {
    const metrics = (window as unknown as MetricsWindow).__pmMetrics;
    const known = new Set(Object.keys(metrics.streams).map((url) => new URL(url, location.href).pathname));
    const newReplay = () => Object.entries(metrics.streams)
      .find(([url, stream]) => !known.has(new URL(url, location.href).pathname) && stream.replayEnds.length > 0);
    const button = [...document.querySelectorAll<HTMLButtonElement>(".terminal-tabs button")]
      .find((candidate) => candidate.textContent?.trim() === "+ Shell");
    if (!button) throw new Error("missing shell creation button");
    const started = performance.now();
    button.click();
    const deadline = started + timeout;
    while (!newReplay()) {
      if (performance.now() >= deadline) throw new Error("shell replay timed out");
      await new Promise(requestAnimationFrame);
    }
    const [url, stream] = newReplay()!;
    const id = new URL(url, location.href).pathname.split("/").at(-1)!;
    await (window as unknown as MetricsWindow).__pmPaint.reveal(`t:${id}`, timeout);
    return stream.replayEnds[0] - started;
  }, { timeout: MEASURE_TIMEOUT_MS });
}

async function returnToAgentPaint(page: Page, key: string): Promise<number> {
  return page.evaluate(async ({ key, timeout }) => {
    const agent = [...document.querySelectorAll<HTMLButtonElement>(".terminal-tabs button")]
      .find((candidate) => candidate.textContent?.trim() === "agent");
    if (!agent) throw new Error("missing agent tab");
    const started = performance.now();
    agent.click();
    await (window as unknown as MetricsWindow).__pmPaint.reveal(key, timeout);
    return performance.now() - started;
  }, { key, timeout: MEASURE_TIMEOUT_MS });
}

test.describe("terminal responsiveness", () => {
  test("many shell creations keep the control path responsive", async ({ page }) => {
    const errors = await openSession(page);
    const agentKey = await page.evaluate(() => (window as unknown as MetricsWindow).__pmPaint.visibleKey());
    if (!agentKey) throw new Error("missing selected agent layer");
    const agentUrl = await page.evaluate(() => Object.entries((window as unknown as MetricsWindow).__pmMetrics.streams)
      .filter(([, stream]) => stream.opened > 0 && stream.closed === 0 && stream.replayEnds.length > 0).at(-1)![0]);
    await resetLongTasks(page);
    const samples: number[] = [];
    for (let index = 0; index < SHELL_COUNT; index += 1) {
      samples.push(await createShellToReplay(page));
    }
    const creation = summary(samples);
    const agentReturnMs = await returnToAgentPaint(page, agentKey);
    const browserMetrics = await page.evaluate(() => (window as unknown as MetricsWindow).__pmMetrics);
    const longestTaskMs = Math.max(0, ...browserMetrics.longTasks.map((task) => task.duration));
    console.log(JSON.stringify({ creation, samples, agentReturnMs, longestTaskMs }));
    expect(await page.locator(".terminal-tab").count()).toBe(SHELL_COUNT);
    expect(browserMetrics.streams[agentUrl].created).toBe(1);
    expect(browserMetrics.streams[agentUrl].closeCodes).toEqual([]);
    expect(creation.p99).toBeLessThanOrEqual(SHELL_CREATE_P99_BUDGET_MS);
    expect(agentReturnMs).toBeLessThanOrEqual(AGENT_RETURN_BUDGET_MS);
    expect(longestTaskMs).toBeLessThanOrEqual(LONG_TASK_BUDGET_MS);
    expect(errors).toEqual([]);
  });

  test("warm terminal switches paint within two frames", async ({ page }) => {
    const errors = await openSession(page);
    const agentKey = await page.evaluate(() => (window as unknown as MetricsWindow).__pmPaint.visibleKey());
    await createShellToReplay(page);
    const shellKey = await page.evaluate(() => (window as unknown as MetricsWindow).__pmPaint.visibleKey());
    if (!agentKey || !shellKey) throw new Error("missing measured terminal layers");
    const shell = page.locator(".terminal-tab").last();
    await shell.click();
    await page.waitForFunction(() => document.querySelector(".terminal-tab.active") !== null);
    await page.getByRole("button", { name: "agent", exact: true }).click();
    await page.evaluate(async ({ key, timeout }) => {
      await (window as unknown as MetricsWindow).__pmPaint.reveal(key, timeout);
    }, { key: agentKey, timeout: MEASURE_TIMEOUT_MS });
    await resetLongTasks(page);
    const createdBefore = await page.evaluate(() => Object.values(
      (window as unknown as MetricsWindow).__pmMetrics.streams,
    ).reduce((count, stream) => count + stream.created, 0));
    const samples = await page.evaluate(async ({ count, agentKey, shellKey, timeout }) => {
      const buttons = [...document.querySelectorAll<HTMLButtonElement>(".terminal-tabs button")];
      const agent = buttons.find((button) => button.textContent?.trim() === "agent");
      const shells = [...document.querySelectorAll<HTMLButtonElement>(".terminal-tab")];
      const shell = shells.at(-1);
      if (!agent || !shell) throw new Error("missing terminal tabs");
      const measured: number[] = [];
      for (let index = 0; index < count; index += 1) {
        const target = index % 2 === 0 ? shell : agent;
        const started = performance.now();
        target.click();
        await (window as unknown as MetricsWindow).__pmPaint.reveal(index % 2 === 0 ? shellKey : agentKey, timeout);
        measured.push(performance.now() - started);
      }
      return measured;
    }, { count: SWITCH_COUNT, agentKey, shellKey, timeout: MEASURE_TIMEOUT_MS });
    const switching = summary(samples);
    const browserMetrics = await page.evaluate(() => (window as unknown as MetricsWindow).__pmMetrics);
    const createdAfter = Object.values(browserMetrics.streams)
      .reduce((count, stream) => count + stream.created, 0);
    const longestTaskMs = Math.max(0, ...browserMetrics.longTasks.map((task) => task.duration));
    console.log(JSON.stringify({ switching, longestTaskMs }));
    expect(switching.p99).toBeLessThanOrEqual(WARM_SWITCH_P99_BUDGET_MS);
    expect(createdAfter).toBe(createdBefore);
    expect(longestTaskMs).toBeLessThanOrEqual(LONG_TASK_BUDGET_MS);
    expect(errors).toEqual([]);
  });

  test("closing shells removes their tabs promptly", async ({ page }) => {
    const errors = await openSession(page);
    for (let index = 0; index < CLOSE_COUNT; index += 1) await createShellToReplay(page);
    await resetLongTasks(page);
    const samples: number[] = [];
    for (let index = 0; index < CLOSE_COUNT; index += 1) {
      await page.locator(".terminal-tab").last().click();
      await page.getByRole("button", { name: "close", exact: true }).waitFor();
      samples.push(await page.evaluate(async ({ timeout }) => {
        const before = document.querySelectorAll(".terminal-tab").length;
        const close = [...document.querySelectorAll<HTMLButtonElement>(".terminal-tabs button")]
          .find((button) => button.textContent?.trim() === "close");
        if (!close) throw new Error("missing shell close button");
        const started = performance.now();
        close.click();
        const deadline = started + timeout;
        while (document.querySelectorAll(".terminal-tab").length === before) {
          if (performance.now() >= deadline) throw new Error("shell close timed out");
          await new Promise(requestAnimationFrame);
        }
        await new Promise(requestAnimationFrame);
        return performance.now() - started;
      }, { timeout: MEASURE_TIMEOUT_MS }));
    }
    const closing = summary(samples);
    const longestTaskMs = await page.evaluate(() => Math.max(
      0,
      ...(window as unknown as MetricsWindow).__pmMetrics.longTasks.map((task) => task.duration),
    ));
    console.log(JSON.stringify({ closing, longestTaskMs }));
    expect(closing.p99).toBeLessThanOrEqual(CLOSE_P99_BUDGET_MS);
    expect(longestTaskMs).toBeLessThanOrEqual(LONG_TASK_BUDGET_MS);
    expect(errors).toEqual([]);
  });

  test("an output burst does not block another terminal or control", async ({ page }) => {
    const errors = await openSession(page);
    await resetLongTasks(page);
    const initial = await page.evaluate(() => (window as unknown as MetricsWindow).__pmMetrics);
    const agentKey = await page.evaluate(() => (window as unknown as MetricsWindow).__pmPaint.visibleKey());
    if (!agentKey) throw new Error("missing selected agent layer");
    const agentUrl = Object.keys(initial.streams)[0];
    const initialBytes = initial.streams[agentUrl].bytes;
    const initialControlCloses = initial.controlClosed;
    await page.evaluate(({ url, triggerBytes }) => {
      const metrics = (window as unknown as MetricsWindow).__pmMetrics;
      metrics.floodAction = {
        url,
        triggerBytes,
        knownUrls: Object.keys(metrics.streams),
      };
    }, { url: agentUrl, triggerBytes: initialBytes + FLOOD_SWITCH_AFTER_BYTES });
    const textarea = page.locator(".term-layer:visible textarea");
    await textarea.focus();
    await page.keyboard.type(`bigout ${OUTPUT_BURST_BYTES}`);
    await page.keyboard.press("Enter");
    const otherTerminal = await page.evaluate(async ({ timeout }) => {
      const metrics = (window as unknown as MetricsWindow).__pmMetrics;
      const deadline = performance.now() + timeout;
      while (true) {
        const action = metrics.floodAction;
        const known = new Set(action?.knownUrls ?? []);
        const replayed = Object.entries(metrics.streams)
          .filter(([url]) => !known.has(url))
          .flatMap(([, stream]) => stream.replayEnds);
        if (action?.started !== undefined && replayed.length > 0) {
          const [url, stream] = Object.entries(metrics.streams)
            .find(([url, stream]) => !known.has(url) && stream.replayEnds.length > 0)!;
          const id = new URL(url, location.href).pathname.split("/").at(-1)!;
          await (window as unknown as MetricsWindow).__pmPaint.reveal(`t:${id}`, timeout);
          return { url, key: `t:${id}`, replayMs: Math.min(...stream.replayEnds) - action.started };
        }
        if (performance.now() >= deadline) throw new Error("other terminal replay timed out");
        await new Promise(requestAnimationFrame);
      }
    }, { timeout: MEASURE_TIMEOUT_MS });
    await textarea.focus();
    const markerStarted = await page.evaluate(() => performance.now());
    await page.keyboard.type("printf '\\105\\062\\105\\055\\122\\105\\101\\104\\131\\n'");
    await page.keyboard.press("Enter");
    await page.waitForFunction((url) => (
      (window as unknown as MetricsWindow).__pmMetrics.streams[url]?.tail.includes("E2E-READY")
    ), otherTerminal.url);
    const markerReceivedMs = await page.evaluate((started) => performance.now() - started, markerStarted);
    await page.evaluate(async ({ key, timeout }) => {
      await (window as unknown as MetricsWindow).__pmPaint.marker(key, "E2E-READY", timeout);
    }, { key: otherTerminal.key, timeout: MEASURE_TIMEOUT_MS });
    const markerMs = await page.evaluate((started) => performance.now() - started, markerStarted);
    const agentReturn = await page.evaluate(async ({ url, key, timeout }) => {
      const metrics = (window as unknown as MetricsWindow).__pmMetrics;
      const beforeCreated = metrics.streams[url].created;
      const beforeReplay = metrics.streams[url].replayEnds.length;
      const agent = [...document.querySelectorAll<HTMLButtonElement>(".terminal-tabs button")]
        .find((button) => button.textContent?.trim() === "agent");
      if (!agent) throw new Error("missing agent tab");
      const started = performance.now();
      agent.click();
      await (window as unknown as MetricsWindow).__pmPaint.marker(key, "x".repeat(32), timeout);
      return {
        ms: performance.now() - started,
        reusedSocket: metrics.streams[url].created === beforeCreated,
        replayEndsAdded: metrics.streams[url].replayEnds.length - beforeReplay,
      };
    }, { url: agentUrl, key: agentKey, timeout: MEASURE_TIMEOUT_MS });
    await page.waitForFunction(({ url, initialBytes, outputBytes }) => (
      (window as unknown as MetricsWindow).__pmMetrics.streams[url].bytes - initialBytes >= outputBytes
    ), { url: agentUrl, initialBytes, outputBytes: OUTPUT_BURST_BYTES }, { timeout: MEASURE_TIMEOUT_MS });
    const finalMetrics = await page.evaluate(() => (window as unknown as MetricsWindow).__pmMetrics);
    const agentBytes = finalMetrics.streams[agentUrl].bytes - initialBytes;
    const longestTaskMs = Math.max(0, ...finalMetrics.longTasks.map((task) => task.duration));
    console.log(JSON.stringify({
      agentBytes,
      agentDisconnects: finalMetrics.streams[agentUrl].closed,
      otherTerminalReplayMs: otherTerminal.replayMs,
      markerMs,
      markerReceivedMs,
      agentReturnMs: agentReturn.ms,
      agentReturnReusedSocket: agentReturn.reusedSocket,
      agentReturnReplayEndsAdded: agentReturn.replayEndsAdded,
      longestTaskMs,
      longTasks: finalMetrics.longTasks,
    }));
    expect(agentBytes).toBeGreaterThanOrEqual(OUTPUT_BURST_BYTES);
    expect(finalMetrics.streams[agentUrl].created).toBe(initial.streams[agentUrl].created);
    expect(finalMetrics.streams[agentUrl].closeCodes).toEqual(initial.streams[agentUrl].closeCodes);
    expect(otherTerminal.replayMs).toBeLessThanOrEqual(FLOOD_OTHER_TERMINAL_BUDGET_MS);
    expect(markerMs).toBeLessThanOrEqual(FLOOD_OTHER_TERMINAL_BUDGET_MS);
    expect(agentReturn.ms).toBeLessThanOrEqual(FLOODED_AGENT_RETURN_BUDGET_MS);
    expect(finalMetrics.controlClosed).toBe(initialControlCloses);
    expect(longestTaskMs).toBeLessThanOrEqual(LONG_TASK_BUDGET_MS);
    expect(errors).toEqual([]);
  });
});
