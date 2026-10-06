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

type MetricsWindow = Window & { __pmMetrics: BrowserMetrics };

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
    (window as MetricsWindow).__pmMetrics = metrics;
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
    const metrics = (window as MetricsWindow).__pmMetrics;
    return Object.values(metrics.streams).some((stream) => stream.opened > 0 && stream.closed === 0 && stream.replayEnds.length > 0);
  });
  return errors;
}

async function resetLongTasks(page: Page): Promise<void> {
  await page.evaluate(() => {
    (window as MetricsWindow).__pmMetrics.longTasks = [];
  });
}

async function createShellToReplay(page: Page): Promise<number> {
  return page.evaluate(async ({ timeout }) => {
    const metrics = (window as MetricsWindow).__pmMetrics;
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
    const replayed = newReplay()![1].replayEnds[0];
    return replayed - started;
  }, { timeout: MEASURE_TIMEOUT_MS });
}

async function returnToAgentPaint(page: Page): Promise<number> {
  return page.evaluate(async () => {
    const frame = () => new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
    const agent = [...document.querySelectorAll<HTMLButtonElement>(".terminal-tabs button")]
      .find((candidate) => candidate.textContent?.trim() === "agent");
    if (!agent) throw new Error("missing agent tab");
    const started = performance.now();
    agent.click();
    while (!agent.classList.contains("active")) await frame();
    await frame();
    return performance.now() - started;
  });
}

test.describe("terminal responsiveness", () => {
  test("many shell creations keep the control path responsive", async ({ page }) => {
    const errors = await openSession(page);
    const agentUrl = await page.evaluate(() => Object.entries((window as MetricsWindow).__pmMetrics.streams)
      .filter(([, stream]) => stream.opened > 0 && stream.closed === 0 && stream.replayEnds.length > 0).at(-1)![0]);
    await resetLongTasks(page);
    const samples: number[] = [];
    for (let index = 0; index < SHELL_COUNT; index += 1) {
      samples.push(await createShellToReplay(page));
    }
    const creation = summary(samples);
    const agentReturnMs = await returnToAgentPaint(page);
    const browserMetrics = await page.evaluate(() => (window as MetricsWindow).__pmMetrics);
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
    await createShellToReplay(page);
    const shell = page.locator(".terminal-tab").last();
    await shell.click();
    await page.waitForFunction(() => document.querySelector(".terminal-tab.active") !== null);
    await page.getByRole("button", { name: "agent", exact: true }).click();
    await resetLongTasks(page);
    const createdBefore = await page.evaluate(() => Object.values(
      (window as MetricsWindow).__pmMetrics.streams,
    ).reduce((count, stream) => count + stream.created, 0));
    const samples = await page.evaluate(async ({ count }) => {
      const frame = () => new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
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
        while (!target.classList.contains("active")) await frame();
        await frame();
        measured.push(performance.now() - started);
      }
      return measured;
    }, { count: SWITCH_COUNT });
    const switching = summary(samples);
    const browserMetrics = await page.evaluate(() => (window as MetricsWindow).__pmMetrics);
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
      ...(window as MetricsWindow).__pmMetrics.longTasks.map((task) => task.duration),
    ));
    console.log(JSON.stringify({ closing, longestTaskMs }));
    expect(closing.p99).toBeLessThanOrEqual(CLOSE_P99_BUDGET_MS);
    expect(longestTaskMs).toBeLessThanOrEqual(LONG_TASK_BUDGET_MS);
    expect(errors).toEqual([]);
  });

  test("an output burst does not block another terminal or control", async ({ page }) => {
    const errors = await openSession(page);
    await resetLongTasks(page);
    const initial = await page.evaluate(() => (window as MetricsWindow).__pmMetrics);
    const agentUrl = Object.keys(initial.streams)[0];
    const initialBytes = initial.streams[agentUrl].bytes;
    const initialControlCloses = initial.controlClosed;
    await page.evaluate(({ url, triggerBytes }) => {
      const metrics = (window as MetricsWindow).__pmMetrics;
      metrics.floodAction = {
        url,
        triggerBytes,
        knownUrls: Object.keys(metrics.streams),
      };
    }, { url: agentUrl, triggerBytes: initialBytes + FLOOD_SWITCH_AFTER_BYTES });
    const textarea = page.locator(".term-layer:visible .xterm-helper-textarea");
    await textarea.focus();
    await page.keyboard.type(`bigout ${OUTPUT_BURST_BYTES}`);
    await page.keyboard.press("Enter");
    const otherTerminal = await page.evaluate(async ({ timeout }) => {
      const metrics = (window as MetricsWindow).__pmMetrics;
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
          return { url, replayMs: Math.min(...stream.replayEnds) - action.started };
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
      (window as MetricsWindow).__pmMetrics.streams[url]?.tail.includes("E2E-READY")
    ), otherTerminal.url);
    const markerMs = await page.evaluate((started) => performance.now() - started, markerStarted);
    const agentReturnReplayMs = await page.evaluate(async ({ url, timeout }) => {
      const metrics = (window as MetricsWindow).__pmMetrics;
      const beforeCreated = metrics.streams[url].created;
      const beforeReplay = metrics.streams[url].replayEnds.length;
      const agent = [...document.querySelectorAll<HTMLButtonElement>(".terminal-tabs button")]
        .find((button) => button.textContent?.trim() === "agent");
      if (!agent) throw new Error("missing agent tab");
      const started = performance.now();
      agent.click();
      const deadline = started + timeout;
      while (metrics.streams[url].created === beforeCreated
        || metrics.streams[url].replayEnds.length === beforeReplay) {
        if (performance.now() >= deadline) throw new Error("flooded agent replay timed out");
        await new Promise(requestAnimationFrame);
      }
      await new Promise(requestAnimationFrame);
      return performance.now() - started;
    }, { url: agentUrl, timeout: MEASURE_TIMEOUT_MS });
    const finalMetrics = await page.evaluate(() => (window as MetricsWindow).__pmMetrics);
    const agentBytes = finalMetrics.streams[agentUrl].bytes - initialBytes;
    const longestTaskMs = Math.max(0, ...finalMetrics.longTasks.map((task) => task.duration));
    console.log(JSON.stringify({
      agentBytes,
      agentDisconnects: finalMetrics.streams[agentUrl].closed,
      otherTerminalReplayMs: otherTerminal.replayMs,
      markerMs,
      agentReturnReplayMs,
      longestTaskMs,
      longTasks: finalMetrics.longTasks,
    }));
    expect(agentBytes).toBeGreaterThan(FLOOD_SWITCH_AFTER_BYTES);
    expect(finalMetrics.streams[agentUrl].closed).toBeGreaterThan(0);
    expect(otherTerminal.replayMs).toBeLessThanOrEqual(FLOOD_OTHER_TERMINAL_BUDGET_MS);
    expect(markerMs).toBeLessThanOrEqual(FLOOD_OTHER_TERMINAL_BUDGET_MS);
    expect(agentReturnReplayMs).toBeLessThanOrEqual(FLOODED_AGENT_RETURN_BUDGET_MS);
    expect(finalMetrics.controlClosed).toBe(initialControlCloses);
    expect(longestTaskMs).toBeLessThanOrEqual(LONG_TASK_BUDGET_MS);
    expect(errors).toEqual([]);
  });
});
