import { expect, test, type Page } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";

// Every warm terminal kept drawing while hidden, so a fleet of busy agents
// held the page's main thread and GPU and a switch waited seconds behind
// them. A hidden layer parses its output and draws nothing until shown,
// and is drawn again by the same frame that shows it.

const HISTORY_LINES = 200;
const FRAMES = 100_000;
const FRAME_DELAY_MS = 30;
const LINE_LEN = 80;
const HIDDEN_WINDOW_MS = 1_000;

interface RenderProbe {
  renders: number;
  rendersByFirstShownFrame: number | null;
}

interface ProbeWindow extends Window {
  __renderProbe?: RenderProbe;
  __pmStage?: {
    visibleId: string | null;
    layers: Map<string, {
      el: HTMLElement;
      term: {
        onRender(listener: () => void): { dispose(): void };
        buffer: { active: { length: number } };
      };
    }>;
  };
}

async function selectSession(page: Page, index: number): Promise<string> {
  const row = page.locator(".sb-session").nth(index);
  await row.click();
  await expect(row).toHaveClass(/is-selected/);
  return expectTerminalRevealed(page, "s:");
}

function bufferLength(page: Page, key: string): Promise<number> {
  return page.evaluate((layerKey) =>
    (window as ProbeWindow).__pmStage!.layers.get(layerKey)!.term.buffer.active.length, key);
}

test("a hidden busy terminal draws nothing and is drawn fresh by the frame that shows it", async ({ page }) => {
  await logIn(page);
  const streaming = await selectSession(page, 0);
  await page.locator('.term-layer[style*="visible"] .xterm-helper-textarea').focus();
  await page.keyboard.type(`claudestream ${HISTORY_LINES} ${FRAMES} ${FRAME_DELAY_MS} ${LINE_LEN}`);
  await page.keyboard.press("Enter");
  await expect.poll(() => bufferLength(page, streaming)).toBeGreaterThan(HISTORY_LINES);

  await selectSession(page, 1);
  await page.evaluate((key) => {
    const layer = (window as ProbeWindow).__pmStage!.layers.get(key)!;
    const probe: RenderProbe = { renders: 0, rendersByFirstShownFrame: null };
    (window as ProbeWindow).__renderProbe = probe;
    layer.term.onRender(() => probe.renders += 1);
    let shown = false;
    new MutationObserver(() => {
      if (shown || layer.el.style.visibility !== "visible") return;
      shown = true;
      // Runs in the first frame painted with the layer shown, after xterm's
      // own frame callback for anything it was asked to draw before then.
      requestAnimationFrame(() => probe.rendersByFirstShownFrame = probe.renders);
    }).observe(layer.el, { attributes: true, attributeFilter: ["style"] });
  }, streaming);
  const parsedBefore = await bufferLength(page, streaming);
  // e2e-real-time-wait: the contract is that a hidden terminal draws nothing over time while its agent prints.
  await page.waitForTimeout(HIDDEN_WINDOW_MS);
  expect(await bufferLength(page, streaming), "the hidden terminal still parses its output").toBeGreaterThan(parsedBefore);
  expect(await page.evaluate(() => (window as ProbeWindow).__renderProbe!.renders)).toBe(0);

  expect(await selectSession(page, 0)).toBe(streaming);
  await expect.poll(() => page.evaluate(() => (window as ProbeWindow).__renderProbe!.rendersByFirstShownFrame))
    .not.toBeNull();
  expect(
    await page.evaluate(() => (window as ProbeWindow).__renderProbe!.rendersByFirstShownFrame),
    "the frame that shows the terminal also draws its current screen",
  ).toBeGreaterThan(0);
});
