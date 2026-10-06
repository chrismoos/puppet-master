import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

// Switching between agent sessions left the screen wrong: typed input drawn
// above the input box, and lines overlapping each other. claudestream models
// the real agent contract — a full redraw on SIGWINCH, incremental frames
// otherwise — and numbers every history line, so a screen that duplicated,
// dropped or reordered anything says so arithmetically instead of visually.

const HISTORY_LINES = 300;
const FRAMES = 600;
const FRAME_DELAY_MS = 25;
const LINE_LEN = 80;

interface StageWindow extends Window {
  __pmStage?: {
    debugSnapshot(): Array<{ key: string; visible: boolean }>;
    layers: Map<string, {
      term: {
        rows: number;
        buffer: {
          active: {
            baseY: number;
            length: number;
            getLine(row: number): { translateToString(trimRight?: boolean): string } | undefined;
          };
        };
      };
    }>;
  };
}

/** The rows currently on screen for the visible layer, oldest first. */
async function visibleRows(page: Page, key: string): Promise<string[]> {
  return page.evaluate((layerKey) => {
    const stage = (window as StageWindow).__pmStage;
    const layer = stage?.layers.get(layerKey);
    if (!layer) return [];
    const buffer = layer.term.buffer.active;
    const rows: string[] = [];
    for (let row = buffer.baseY; row < buffer.baseY + layer.term.rows; row += 1) {
      rows.push(buffer.getLine(row)?.translateToString(true) ?? "");
    }
    return rows;
  }, key);
}

/** History line numbers in the order they appear down the screen. */
function historyNumbers(rows: string[]): number[] {
  const found: number[] = [];
  for (const row of rows) {
    const match = /CLAUDE-HISTORY-(\d+)/.exec(row);
    if (match) found.push(Number(match[1]));
  }
  return found;
}

async function visibleLayerKey(page: Page): Promise<string> {
  return page.evaluate(() => {
    const stage = (window as StageWindow).__pmStage;
    return stage?.debugSnapshot().find((layer) => layer.visible)?.key ?? "";
  });
}

test("switching away from a streaming agent and back leaves the screen consistent", async ({ page }) => {
  await logIn(page);
  const sessions = page.locator(".sb-session");
  await expect(sessions.nth(1)).toBeVisible();

  await sessions.nth(0).click();
  await expect(page.locator(".xterm")).toBeVisible();

  const textarea = page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
  await textarea.focus();
  await page.keyboard.type(`claudestream ${HISTORY_LINES} ${FRAMES} ${FRAME_DELAY_MS} ${LINE_LEN}`);
  await page.keyboard.press("Enter");

  const key = await visibleLayerKey(page);
  expect(key, "the visible layer is identifiable").not.toBe("");

  await expect
    .poll(async () => historyNumbers(await visibleRows(page, key)).length, { timeout: 20_000 })
    .toBeGreaterThan(3);

  await sessions.nth(1).click();
  await expect(sessions.nth(1)).toHaveClass(/is-selected/);
  // Let the agent keep rendering into a terminal nobody is watching.
  await expect
    .poll(async () => (await visibleLayerKey(page)) !== key, { timeout: 10_000 })
    .toBe(true);

  await sessions.nth(0).click();
  await expect(sessions.nth(0)).toHaveClass(/is-selected/);
  await expect.poll(async () => visibleLayerKey(page), { timeout: 10_000 }).toBe(key);

  // Give the redraw every chance to settle before judging it.
  await expect
    .poll(async () => historyNumbers(await visibleRows(page, key)).length, { timeout: 20_000 })
    .toBeGreaterThan(3);

  const rows = await visibleRows(page, key);
  const numbers = historyNumbers(rows);

  const duplicates = numbers.filter((n, i) => numbers.indexOf(n) !== i);
  expect(
    duplicates,
    `the same history line is on screen twice, which is what overlapping text looks like:\n${rows.join("\n")}`,
  ).toEqual([]);

  const ascending = numbers.every((n, i) => i === 0 || n > numbers[i - 1]);
  expect(
    ascending,
    `history lines are out of order down the screen:\n${rows.join("\n")}`,
  ).toBe(true);
});
