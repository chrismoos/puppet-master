import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

// Switching sessions must replace one finished screen with another. A session
// whose snapshot is still on its way, or one that has to be redrawn at a new
// size, used to show up empty, or at its old size, and then fill in. Only a
// real browser shows this, and only per animation frame: the sampler records
// what is on screen every frame across the switch.

const HISTORY_LINES = 3_000;
const LINE_LEN = 120;

interface Sample {
  onScreen: string[];
  layers: Record<string, { painted: boolean; lines: number; cols: number; rows: number }>;
}

interface StageWindow extends Window {
  __switchSamples: Sample[];
  __switchSampling: boolean;
  __pmStage: {
    layers: Map<string, {
      el: HTMLElement;
      replayPainted: boolean;
      initialReplayPending: boolean;
      swapPending: boolean;
      term: { cols: number; rows: number; buffer: { active: { length: number } } };
    }>;
  };
}

async function fillHistory(page: Page): Promise<void> {
  const textarea = page.locator('.term-layer[style*="visibility: visible"] .xterm-helper-textarea');
  await textarea.focus();
  await page.keyboard.type(`claudestream ${HISTORY_LINES} 1 0 ${LINE_LEN}`);
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
    const tick = () => {
      if (!w.__switchSampling) return;
      const sample: Sample = { onScreen: [], layers: {} };
      for (const [key, layer] of w.__pmStage.layers) {
        if (layer.el.style.visibility === "visible") sample.onScreen.push(key);
        sample.layers[key] = {
          painted: layer.replayPainted && !layer.initialReplayPending && !layer.swapPending,
          lines: layer.term.buffer.active.length,
          cols: layer.term.cols,
          rows: layer.term.rows,
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

/** Every frame shows one finished screen: the previous session, or the next
 * one exactly as it ends up. */
function expectOneSwap(result: { from: string; to: string; samples: Sample[] }): void {
  const { from, to, samples } = result;
  const final = samples.at(-1)!.layers[to];
  let swapped = false;
  for (const [index, sample] of samples.entries()) {
    expect(sample.onScreen, `frame ${index}`).toHaveLength(1);
    const [shown] = sample.onScreen;
    if (shown === to) {
      swapped = true;
      expect(sample.layers[to], `frame ${index}`).toEqual(final);
    } else {
      expect(shown, `frame ${index}`).toBe(from);
      expect(swapped, `frame ${index} went back to the previous session`).toBe(false);
    }
  }
  expect(swapped).toBe(true);
}

test("a session whose snapshot is slow to arrive replaces the previous screen in one step", async ({ page }) => {
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
  expectOneSwap(result);
  // The previous screen stayed up while the snapshot travelled.
  expect(result.samples.filter((sample) => sample.onScreen[0] === result.from).length).toBeGreaterThan(5);
});

test("a warm session at a stale size is shown only once it is redrawn at the pane's size", async ({ page }) => {
  await logIn(page);
  await openSession(page, 0);
  await fillHistory(page);
  await openSession(page, 1);
  await fillHistory(page);

  const shownCols = () => page.evaluate(() => {
    const stage = (window as unknown as StageWindow).__pmStage;
    return [...stage.layers.values()].find((layer) => layer.el.style.visibility === "visible")!.term.cols;
  });
  const colsBefore = await shownCols();
  const before = page.viewportSize()!;
  await page.setViewportSize({ width: before.width - 200, height: before.height - 120 });
  await expect.poll(shownCols).toBeLessThan(colsBefore);

  const settled = await switchAndSample(page);
  expectOneSwap(settled);
  const shown = settled.samples.at(-1)!.layers[settled.to];
  const other = settled.samples.at(-1)!.layers[settled.from];
  expect({ cols: shown.cols, rows: shown.rows }).toEqual({ cols: other.cols, rows: other.rows });
});
