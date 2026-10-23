import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

const HISTORY_LINES = 3_000;
const LINE_LEN = 120;
/// Enough frames that the agent is still printing when the test ends.
const STREAMING_FRAMES = 100_000;
const STREAMING_FRAME_MS = 40;

interface Sample {
  onScreen: string[];
  layers: Record<string, { painted: boolean; lines: number; cols: number; rows: number }>;
}

interface StageWindow extends Window {
  __switchSamples: Sample[];
  __switchSampling: boolean;
  __switchRenderListeners: Array<{ dispose(): void }>;
  __pmStage: {
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
  const textarea = page.locator('.term-layer[style*="visibility: visible"] .xterm-helper-textarea');
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
      const sample: Sample = { onScreen: [], layers: {} };
      for (const [key, layer] of w.__pmStage.layers) {
        if (!observed.has(key)) {
          observed.add(key);
          w.__switchRenderListeners.push(layer.term.onRender(() => record(key)));
          layer.term.refresh(0, layer.term.rows - 1);
        }
        if (layer.el.style.visibility === "visible") sample.onScreen.push(key);
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

async function visibleKey(page: Page): Promise<string> {
  return page.evaluate(() => {
    const stage = (window as unknown as StageWindow).__pmStage;
    return [...stage.layers.entries()].find(([, layer]) => layer.el.style.visibility === "visible")![0];
  });
}

async function switchAndSample(page: Page): Promise<{ from: string; to: string; samples: Sample[] }> {
  const from = await visibleKey(page);
  await startSampling(page);
  await page.locator(".sb-session:not(.is-selected)").first().click();
  await expect.poll(async () => {
    const key = await visibleKey(page);
    if (key === from) return false;
    return page.evaluate((k) => {
      const layer = (window as unknown as StageWindow).__pmStage.layers.get(k)!;
      return layer.replayPainted && !layer.initialReplayPending && !layer.swapPending;
    }, key);
  }, { timeout: 20_000 }).toBe(true);
  // e2e-real-time-wait: frames after the swap would show a late refill
  await page.waitForTimeout(500);
  const samples = await stopSampling(page);
  return { from, to: await visibleKey(page), samples };
}

/// The pane goes from the session being left, to this session's own empty
/// terminal, to its snapshot in one step. No frame shows a partly drawn
/// screen, and none shows the session being left once the switch has begun.
function expectBlankThenOnePaint(result: { from: string; to: string; samples: Sample[] }): void {
  const { from, to, samples } = result;
  const final = samples.at(-1)!.layers[to];
  expect(final.painted).toBe(true);
  const states: string[] = [];
  for (const [index, sample] of samples.entries()) {
    expect(sample.onScreen, `frame ${index}`).toHaveLength(1);
    const [shown] = sample.onScreen;
    if (shown !== from && shown !== to) throw new Error(`frame ${index} shows ${shown}`);
    const state = shown === from
      ? "previous"
      : sample.layers[to]?.painted ? "painted" : "blank";
    if (states.at(-1) !== state) states.push(state);
  }
  expect(states).toEqual(["previous", "blank", "painted"]);
}

/// The pane goes from the session being left straight to the session asked
/// for, before that session's redraw at the pane's size has arrived, and
/// never goes back.
function expectImmediateSwap(result: { from: string; to: string; samples: Sample[] }): void {
  const { from, to, samples } = result;
  const final = samples.at(-1)!.layers[to];
  expect(final.painted).toBe(true);
  let firstShown: Sample | null = null;
  for (const [index, sample] of samples.entries()) {
    expect(sample.onScreen, `frame ${index}`).toHaveLength(1);
    const [shown] = sample.onScreen;
    if (shown === to) {
      firstShown ??= sample;
    } else {
      expect(shown, `frame ${index}`).toBe(from);
      expect(firstShown, `frame ${index} went back to the previous session`).toBeNull();
    }
  }
  expect(firstShown).not.toBeNull();
  expect(firstShown!.layers[to]?.painted, "the switch waited for the redraw").toBe(false);
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
  expectBlankThenOnePaint(result);
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

test("a warm session at a stale size takes the pane at once and is redrawn at the pane's size", async ({ page }) => {
  await logIn(page);
  await openSession(page, 0);
  await fillHistory(page);
  await openSession(page, 1);
  await fillHistory(page);
  await shrinkViewport(page);

  const settled = await switchAndSample(page);
  expectImmediateSwap(settled);
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
  expectImmediateSwap(settled);
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

test("a session still reaching for its terminal says it is connecting over an empty pane", async ({ page }) => {
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
  const pane = page.locator('.term-layer[style*="visibility: visible"]');
  await expect(pane.locator(".terminal-stream-status.is-connecting")).toHaveText("connecting…");
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
  await expect(pane.locator(".terminal-stream-status")).toBeHidden();
  await expect.poll(() => page.evaluate(() => {
    const stage = (window as unknown as StageWindow).__pmStage as unknown as {
      visibleId: string;
      layers: StageWindow["__pmStage"]["layers"];
    };
    return stage.layers.get(stage.visibleId)!.replayPainted;
  })).toBe(true);
});

interface DebugStageWindow extends Window {
  __pmStage: {
    debugSnapshot(): Array<{
      visible: boolean;
      bufferLines: number;
      rows: number;
      viewportY: number;
      socket: { replayCount: number } | null;
    }>;
    layers: Map<string, {
      el: HTMLElement;
      term: {
        rows: number;
        buffer: { active: { length: number; viewportY: number; getLine(index: number): { translateToString(trim: boolean): string } | undefined } };
      };
    }>;
  };
}

function shownHistory(page: Page) {
  return page.evaluate(() => {
    const stage = (window as unknown as DebugStageWindow).__pmStage;
    const shown = stage.debugSnapshot().find((entry) => entry.visible);
    if (!shown) return null;
    const layer = [...stage.layers.values()].find((entry) => entry.el.style.visibility === "visible")!;
    const buffer = layer.term.buffer.active;
    return {
      lines: buffer.length,
      rows: layer.term.rows,
      viewportY: buffer.viewportY,
      replays: shown.socket?.replayCount ?? 0,
    };
  });
}

test("a session opened cold has its whole scrollback once its screen is up", async ({ page }) => {
  await logIn(page);
  await openSession(page, 0);
  await fillHistory(page);

  // A first snapshot carries a few hundred lines, so the screen paints
  // without parsing the scrollback, and the rest follows it.
  await page.reload();
  await expect.poll(() => shownHistory(page).then((state) => state?.lines ?? 0), { timeout: 20_000 })
    .toBeGreaterThan(HISTORY_LINES);

  const filled = (await shownHistory(page))!;
  expect(filled.replays).toBeGreaterThan(1);
  // The history arrives above the screen, which stays at the bottom.
  expect(filled.viewportY).toBe(filled.lines - filled.rows);
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
  const textarea = page.locator('.term-layer[style*="visibility: visible"] .xterm-helper-textarea');
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
