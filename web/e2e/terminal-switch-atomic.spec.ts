import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

const HISTORY_LINES = 3_000;
const LINE_LEN = 120;
/// Enough frames that the agent is still printing when the test ends.
const STREAMING_FRAMES = 100_000;
const STREAMING_FRAME_MS = 40;

interface Sample {
  selected: string | null;
  onScreen: string[];
  /** The shown layer's loading curtain covers it. */
  curtained: boolean;
  layers: Record<string, { painted: boolean; lines: number; cols: number; rows: number }>;
}

interface StageWindow extends Window {
  __switchSamples: Sample[];
  __switchSampling: boolean;
  __switchRenderListeners: Array<{ dispose(): void }>;
  __pmStage: {
    visibleId: string | null;
    theme: {
      background: { r: number; g: number; b: number };
      foreground: { r: number; g: number; b: number };
    };
    layers: Map<string, {
      el: HTMLElement;
      replayPainted: boolean;
      initialReplayPending: boolean;
      swapPending: boolean;
      term: {
        cols: number;
        rows: number;
        buffer: { active: { length: number } };
        modes: { synchronizedOutputMode: boolean };
        onRender(listener: () => void): { dispose(): void };
        refresh(start: number, end: number): void;
      };
    }>;
  };
}

async function fillHistory(page: Page, frames = 1, frameMs = 0): Promise<void> {
  const textarea = page.locator('.term-layer[style*="visibility: visible"] .ghostty-input');
  await textarea.focus();
  await page.keyboard.type(`claudestream ${HISTORY_LINES} ${frames} ${frameMs} ${LINE_LEN}`);
  await page.keyboard.press("Enter");
  await expect.poll(() => page.evaluate(() => {
    const stage = (window as unknown as StageWindow).__pmStage;
    return [...stage.layers.values()].some((layer) =>
      layer.el.style.visibility === "visible" && layer.term.buffer.active.length > 3_000);
  })).toBe(true);
}

async function openSession(page: Page, index: number): Promise<void> {
  const row = page.locator(".sb-session").nth(index);
  await row.click();
  await expect(row).toHaveClass(/is-selected/);
  await expect.poll(() => page.evaluate(() => {
    const stage = (window as unknown as StageWindow).__pmStage as unknown as {
      visibleId: string;
      layers: StageWindow["__pmStage"]["layers"];
    };
    const layer = stage.layers.get(stage.visibleId);
    return Boolean(layer && layer.el.style.visibility === "visible" && layer.replayPainted);
  })).toBe(true);
}

async function startSampling(page: Page): Promise<void> {
  await page.evaluate(() => {
    const w = window as unknown as StageWindow;
    w.__switchSamples = [];
    w.__switchSampling = true;
    const rendered = new Map<string, Omit<Sample["layers"][string], "painted">>();
    const record = (key: string) => {
      const layer = w.__pmStage.layers.get(key)!;
      if (layer.term.modes.synchronizedOutputMode) return;
      rendered.set(key, {
        lines: layer.term.buffer.active.length,
        cols: layer.term.cols,
        rows: layer.term.rows,
      });
    };
    w.__switchRenderListeners = [];
    for (const [key, layer] of w.__pmStage.layers) {
      record(key);
      // Synchronized output changes the buffer before those cells are painted.
      w.__switchRenderListeners.push(layer.term.onRender(() => record(key)));
    }
    const observed = new Set(w.__pmStage.layers.keys());
    const tick = () => {
      if (!w.__switchSampling) return;
      const sample: Sample = { selected: w.__pmStage.visibleId, onScreen: [], curtained: false, layers: {} };
      for (const [key, layer] of w.__pmStage.layers) {
        if (!observed.has(key)) {
          observed.add(key);
          w.__switchRenderListeners.push(layer.term.onRender(() => record(key)));
          layer.term.refresh(0, layer.term.rows - 1);
        }
        if (layer.el.style.visibility === "visible") {
          sample.onScreen.push(key);
          sample.curtained = !layer.el.querySelector<HTMLElement>(".terminal-loading-curtain")!.hidden;
        }
        const painted = rendered.get(key);
        if (painted) sample.layers[key] = {
          ...painted,
          painted: layer.replayPainted && !layer.initialReplayPending && !layer.swapPending,
        };
      }
      w.__switchSamples.push(sample);
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
}

async function stopSampling(page: Page): Promise<Sample[]> {
  return page.evaluate(() => {
    const w = window as unknown as StageWindow;
    w.__switchSampling = false;
    for (const listener of w.__switchRenderListeners) listener.dispose();
    return w.__switchSamples;
  });
}

async function visibleKey(page: Page): Promise<string | null> {
  return page.evaluate(() => {
    const stage = (window as unknown as StageWindow).__pmStage;
    return [...stage.layers.entries()].find(([, layer]) => layer.el.style.visibility === "visible")?.[0] ?? null;
  });
}

async function switchAndSample(page: Page): Promise<{ from: string; to: string; samples: Sample[] }> {
  const from = await visibleKey(page);
  if (!from) throw new Error("outgoing terminal is not visible");
  await startSampling(page);
  await page.locator(".sb-session:not(.is-selected)").first().click();
  await expect.poll(async () => {
    const key = await visibleKey(page);
    if (!key || key === from) return false;
    return page.evaluate((k) => {
      const layer = (window as unknown as StageWindow).__pmStage.layers.get(k)!;
      return layer.replayPainted && !layer.initialReplayPending && !layer.swapPending;
    }, key);
  }, { timeout: 20_000 }).toBe(true);
  // e2e-real-time-wait: frames after the swap would show a late refill
  await page.waitForTimeout(500);
  const samples = await stopSampling(page);
  const to = await visibleKey(page);
  if (!to) throw new Error("incoming terminal is not visible");
  return { from, to, samples };
}

function expectAtomicSwap(result: { from: string; to: string; samples: Sample[] }): void {
  const { from, to, samples } = result;
  expect(samples.at(-1)!.onScreen).toEqual([to]);
  let leftPrevious = false;
  let revealed = false;
  for (const [index, sample] of samples.entries()) {
    expect(sample.onScreen.length, `frame ${index}`).toBeLessThanOrEqual(1);
    const [shown] = sample.onScreen;
    if (shown === from) {
      expect(sample.selected, `frame ${index} shows the outgoing terminal after selection`).toBe(from);
      expect(leftPrevious, `frame ${index} returned to the previous session`).toBe(false);
      continue;
    }
    leftPrevious = true;
    if (!shown) {
      expect(revealed, `frame ${index} hid an already revealed terminal`).toBe(false);
      continue;
    }
    expect(shown, `frame ${index}`).toBe(to);
    expect(sample.layers[to]?.painted, `frame ${index} exposes an unfinished terminal`).toBe(true);
    expect(sample.curtained, `frame ${index}`).toBe(false);
    revealed = true;
  }
  expect(revealed).toBe(true);
}

test("a session whose snapshot is slow to arrive takes the pane empty rather than leaving the previous screen up", async ({ page }) => {
  await logIn(page);
  await openSession(page, 0);
  await fillHistory(page);
  await openSession(page, 1);
  await fillHistory(page);

  const network = await page.context().newCDPSession(page);
  await network.send("Network.enable");
  await network.send("Network.emulateNetworkConditions", {
    offline: false,
    latency: 120,
    downloadThroughput: 600_000,
    uploadThroughput: 200_000,
  });
  await page.reload();
  await expect(page.locator('.term-layer[style*="visibility: visible"]')).toHaveCount(1, { timeout: 20_000 });

  const result = await switchAndSample(page);
  expectAtomicSwap(result);
});

async function shrinkViewport(page: Page): Promise<void> {
  const shownCols = () => page.evaluate(() => {
    const stage = (window as unknown as StageWindow).__pmStage;
    return [...stage.layers.values()].find((layer) => layer.el.style.visibility === "visible")!.term.cols;
  });
  const colsBefore = await shownCols();
  const before = page.viewportSize()!;
  await page.setViewportSize({ width: before.width - 200, height: before.height - 120 });
  await expect.poll(shownCols).toBeLessThan(colsBefore);
}

test("a warm session at a stale size is revealed atomically at the pane's size", async ({ page }) => {
  await logIn(page);
  await openSession(page, 0);
  await fillHistory(page);
  await openSession(page, 1);
  await fillHistory(page);
  await shrinkViewport(page);

  const settled = await switchAndSample(page);
  expectAtomicSwap(settled);
  const shown = settled.samples.at(-1)!.layers[settled.to];
  const other = settled.samples.at(-1)!.layers[settled.from];
  expect({ cols: shown.cols, rows: shown.rows }).toEqual({ cols: other.cols, rows: other.rows });
});

test("a session that never stops printing finishes switching at a new size", async ({ page }) => {
  await logIn(page);
  await openSession(page, 0);
  await fillHistory(page);
  await openSession(page, 1);
  await fillHistory(page, STREAMING_FRAMES, STREAMING_FRAME_MS);
  await openSession(page, 0);
  await shrinkViewport(page);

  const settled = await switchAndSample(page);
  expectAtomicSwap(settled);
});

test("pressing a session row starts attaching its terminal before the click opens it", async ({ page }) => {
  await logIn(page);
  await openSession(page, 0);
  const shownBefore = await visibleKey(page);
  const tickets: string[] = [];
  page.on("request", (request) => {
    if (request.url().includes("/attach-ticket")) tickets.push(request.url());
  });

  const row = page.locator(".sb-session").nth(1);
  await expect(row).not.toHaveClass(/is-selected/);
  const box = (await row.boundingBox())!;
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.down();
  await expect.poll(() => tickets.length).toBe(1);
  const pressed = await page.evaluate(() => {
    const stage = (window as unknown as StageWindow).__pmStage;
    return [...stage.layers.entries()].map(([key, layer]) => ({ key, shown: layer.el.style.visibility === "visible" }));
  });
  expect(pressed.filter((layer) => layer.shown).map((layer) => layer.key)).toEqual([shownBefore]);
  expect(pressed).toHaveLength(2);

  await page.mouse.up();
  await expect(row).toHaveClass(/is-selected/);
  await expect.poll(() => page.evaluate(() => {
    const stage = (window as unknown as StageWindow).__pmStage as unknown as {
      visibleId: string;
      layers: StageWindow["__pmStage"]["layers"];
    };
    const layer = stage.layers.get(stage.visibleId);
    return Boolean(layer && layer.el.style.visibility === "visible" && layer.replayPainted);
  })).toBe(true);
  expect(await visibleKey(page)).not.toBe(shownBefore);
  expect(tickets).toHaveLength(1);
});

test("a session still reaching for its terminal stays hidden with a curtain in the terminal's colors", async ({ page }) => {
  await page.route("**/api/terminals/*/history?*", (route) => route.abort());
  await logIn(page);
  await openSession(page, 0);
  await fillHistory(page);
  const from = await visibleKey(page);

  let letTicketThrough = () => {};
  const held = new Promise<void>((resolve) => {
    letTicketThrough = resolve;
  });
  await page.route("**/api/terminals/*/attach-ticket", async (route) => {
    await held;
    await route.continue();
  });

  await page.locator(".sb-session:not(.is-selected)").first().click();
  await expect(page.locator('.term-layer[style*="visibility: visible"]')).toHaveCount(0);
  const pane = page.locator(".term-layer.is-preparing");
  const curtain = pane.locator(".terminal-loading-curtain");
  await expect(curtain).toHaveText("loading…");
  await expect(curtain.locator("span")).toHaveCSS("opacity", "0.55");
  await expect(pane).toHaveCSS("visibility", "hidden");
  await expect(pane.locator(".terminal-stream-status")).toBeHidden();
  const curtainColors = await curtain.evaluate((el) => {
    const style = getComputedStyle(el);
    return { background: style.backgroundColor, color: style.color };
  });
  const terminalColors = await page.evaluate(() => {
    const theme = (window as unknown as StageWindow).__pmStage.theme;
    const probe = document.createElement("span");
    probe.style.backgroundColor = `rgb(${theme.background.r}, ${theme.background.g}, ${theme.background.b})`;
    probe.style.color = `rgb(${theme.foreground.r}, ${theme.foreground.g}, ${theme.foreground.b})`;
    document.body.appendChild(probe);
    const style = getComputedStyle(probe);
    const colors = { background: style.backgroundColor, color: style.color };
    probe.remove();
    return colors;
  });
  expect(curtainColors).toEqual(terminalColors);
  const waiting = await page.evaluate(() => {
    const stage = (window as unknown as StageWindow).__pmStage as unknown as {
      visibleId: string;
      layers: StageWindow["__pmStage"]["layers"];
    };
    const layer = stage.layers.get(stage.visibleId)!;
    return {
      key: stage.visibleId,
      painted: layer.replayPainted,
      lines: layer.term.buffer.active.length,
    };
  });
  expect(waiting.key).not.toBe(from);
  expect(waiting.painted).toBe(false);
  expect(waiting.lines).toBeLessThan(HISTORY_LINES);

  letTicketThrough();
  await expect(page.locator('.term-layer[style*="visibility: visible"]')).toHaveCount(1);
  await expect(curtain).toBeHidden();
  await expect.poll(() => page.evaluate(() => {
    const stage = (window as unknown as StageWindow).__pmStage as unknown as {
      visibleId: string;
      layers: StageWindow["__pmStage"]["layers"];
    };
    return stage.layers.get(stage.visibleId)!.replayPainted;
  })).toBe(true);
});

interface DebugStageWindow extends Window {
  __pmStage?: {
    debugSnapshot(): Array<{
      visible: boolean;
      bufferLines: number;
      rows: number;
      viewportY: number;
      socket: { replayCount: number } | null;
    }>;
    layers: Map<string, {
      el: HTMLElement;
      history: { hasMore: boolean } | null;
      term: { scrollToLine(line: number): void };
    }>;
  };
}

function shownHistory(page: Page) {
  return page.evaluate(() => {
    const stage = (window as unknown as DebugStageWindow).__pmStage;
    // A reloaded page has not built its stage yet.
    if (!stage) return null;
    const shown = stage.debugSnapshot().find((entry) => entry.visible);
    if (!shown) return null;
    return {
      lines: shown.bufferLines,
      rows: shown.rows,
      viewportY: shown.viewportY,
    };
  });
}

test("a session opened cold fetches older scrollback when the reader scrolls up", async ({ page }) => {
  await logIn(page);
  await openSession(page, 0);
  await fillHistory(page);

  await page.reload();
  await expect.poll(() => shownHistory(page).then((state) => state?.lines ?? 0), { timeout: 20_000 })
    .toBeGreaterThan(0);
  const initial = (await shownHistory(page))!;
  expect(initial.lines).toBeLessThan(HISTORY_LINES);
  expect(initial.viewportY).toBe(initial.lines - initial.rows);
  const MAX_HISTORY_BATCHES = 30;
  for (let batch = 0; batch < MAX_HISTORY_BATCHES; batch += 1) {
    const before = (await shownHistory(page))!;
    const requested = await page.evaluate(() => {
      const stage = (window as unknown as DebugStageWindow).__pmStage;
      const layer = [...stage?.layers.values() ?? []].find((entry) => entry.el.style.visibility === "visible");
      if (!layer?.history?.hasMore) return false;
      layer.term.scrollToLine(0);
      return true;
    });
    if (!requested) break;
    await expect.poll(() => shownHistory(page).then((state) => state?.lines ?? 0)).toBeGreaterThan(before.lines);
  }
  expect((await shownHistory(page))!.lines).toBeGreaterThan(HISTORY_LINES);
});

function sessionRowByTitle(page: Page, title: string) {
  return page.locator(".sb-session-title", { hasText: new RegExp(`^${title}$`) })
    .locator("xpath=ancestor::button[contains(@class, 'sb-session')][1]");
}

test("a session that has ended shows no other session's terminal behind its card", async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("pm.showEnded", "true"));
  await logIn(page);
  const endedRow = sessionRowByTitle(page, "browser-e2e");
  const liveRow = sessionRowByTitle(page, "browser-e2e-two");
  await endedRow.click();
  await expect.poll(() => page.locator('.term-layer[style*="visibility: visible"]').count()).toBe(1);
  const textarea = page.locator('.term-layer[style*="visibility: visible"] .ghostty-input');
  await textarea.focus();
  await page.keyboard.type("exit 7");
  await page.keyboard.press("Enter");
  await expect(endedRow).toContainText("ended ·");

  await liveRow.click();
  await expect(liveRow).toHaveClass(/is-selected/);
  await expect(page.locator('.term-layer[style*="visibility: visible"]')).toHaveCount(1);
  await endedRow.click();
  await expect(endedRow).toHaveClass(/is-selected/);
  await expect(page.locator(".term-overlay-card")).toBeVisible();
  await expect(page.locator('.term-layer[style*="visibility: visible"]')).toHaveCount(0);
});
