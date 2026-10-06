import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

const HISTORY_LINES = 300;
const FRAMES = 400;
const FRAME_DELAY_MS = 25;
const LINE_LEN = 80;

interface StageWindow extends Window {
  __pmStage?: {
    debugSnapshot(): Array<{ key: string; visible: boolean }>;
    layers: Map<string, {
      term: {
        cols: number;
        rows: number;
        buffer: {
          active: {
            baseY: number;
            cursorY: number;
            length: number;
            getLine(row: number): {
              isWrapped: boolean;
              translateToString(trimRight?: boolean): string;
            } | undefined;
          };
        };
      };
    }>;
  };
}

interface Screen {
  rows: string[];
  wrapped: boolean[];
  cols: number;
  cursorY: number;
}

async function visibleScreen(page: Page, key: string): Promise<Screen> {
  return page.evaluate((layerKey) => {
    const stage = (window as StageWindow).__pmStage;
    const layer = stage?.layers.get(layerKey);
    if (!layer) return { rows: [], wrapped: [], cols: 0, cursorY: -1 };
    const buffer = layer.term.buffer.active;
    const rows: string[] = [];
    const wrapped: boolean[] = [];
    for (let row = buffer.baseY; row < buffer.baseY + layer.term.rows; row += 1) {
      const line = buffer.getLine(row);
      rows.push(line?.translateToString(true) ?? "");
      wrapped.push(line?.isWrapped ?? false);
    }
    return { rows, wrapped, cols: layer.term.cols, cursorY: buffer.cursorY };
  }, key);
}

function historyNumbers(rows: string[]): number[] {
  const found: number[] = [];
  for (const row of rows) {
    const match = /CLAUDE-HISTORY-(\d+)/.exec(row);
    if (match) found.push(Number(match[1]));
  }
  return found;
}

function spuriousWraps(screen: Screen): string[] {
  const found: string[] = [];
  for (let i = 1; i < screen.rows.length; i += 1) {
    if (!screen.wrapped[i]) continue;
    const joined = `${screen.rows[i - 1]}${screen.rows[i]}`;
    if (joined.length <= screen.cols) {
      found.push(`${screen.rows[i - 1]} / ${screen.rows[i]}`);
    }
  }
  return found;
}

async function visibleLayerKey(page: Page): Promise<string> {
  return page.evaluate(() => {
    const stage = (window as StageWindow).__pmStage;
    return stage?.debugSnapshot().find((layer) => layer.visible)?.key ?? "";
  });
}

test("claudestream survives a reload without a window resize", async ({ page }) => {
  await logIn(page);
  const sessions = page.locator(".sb-session");
  await expect(sessions.nth(0)).toBeVisible();
  await sessions.nth(0).click();
  await expect(page.locator(".xterm")).toBeVisible();

  const textarea = page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
  await textarea.focus();
  await page.keyboard.type(`claudestream ${HISTORY_LINES} ${FRAMES} ${FRAME_DELAY_MS} ${LINE_LEN}`);
  await page.keyboard.press("Enter");

  const key = await visibleLayerKey(page);
  expect(key, "the visible layer is identifiable").not.toBe("");
  await expect
    .poll(async () => historyNumbers((await visibleScreen(page, key)).rows).length, { timeout: 20_000 })
    .toBeGreaterThan(3);

  const before = historyNumbers((await visibleScreen(page, key)).rows);
  await page.reload();
  await expect(page.locator(".xterm")).toBeVisible();
  await sessions.nth(0).click();
  await expect(page.locator('.term-layer[style*="visible"] .xterm')).toBeVisible();

  const restoredKey = await visibleLayerKey(page);
  await expect
    .poll(async () => historyNumbers((await visibleScreen(page, restoredKey)).rows).length, { timeout: 20_000 })
    .toBeGreaterThan(3);

  const screen = await visibleScreen(page, restoredKey);
  const numbers = historyNumbers(screen.rows);
  const duplicates = numbers.filter((n, i) => numbers.indexOf(n) !== i);
  expect(
    duplicates,
    `reload duplicated history lines:\n${screen.rows.join("\n")}`,
  ).toEqual([]);
  expect(
    numbers.every((n, i) => i === 0 || n > numbers[i - 1]),
    `reload reordered history lines:\n${screen.rows.join("\n")}`,
  ).toBe(true);
  expect(numbers.length, "reload restored the agent frame without a window resize").toBeGreaterThan(0);
  expect(before.length).toBeGreaterThan(0);
  expect(
    spuriousWraps(screen),
    `reload wrapped snapshot cells mid-word:\n${screen.rows.join("\n")}`,
  ).toEqual([]);
  const boxRows = screen.rows.filter((row) => row.includes("input box line"));
  expect(boxRows.length, `composer missing after reload:\n${screen.rows.join("\n")}`).toBeGreaterThan(0);
  expect(
    boxRows.every((row) => /input box line \d+ after \d+/.test(row)),
    `composer line split after reload:\n${screen.rows.join("\n")}`,
  ).toBe(true);
  const lastBox = screen.rows.reduce((last, row, i) => (row.includes("input box line") ? i : last), -1);
  const cursorRow = screen.rows[screen.cursorY] ?? "";
  expect(
    screen.cursorY <= lastBox + 1,
    `cursor sat on a blank line below the composer (cursorY=${screen.cursorY} lastBox=${lastBox} row=${JSON.stringify(cursorRow)}):\n${screen.rows.join("\n")}`,
  ).toBe(true);

  const colsBeforeGrow = screen.cols;
  await page.evaluate(() => {
    const stage = document.querySelector(".term-stage") as HTMLElement | null;
    if (!stage) return;
    const width = stage.getBoundingClientRect().width;
    stage.style.right = "auto";
    stage.style.width = `${width + 400}px`;
  });
  await expect
    .poll(async () => {
      const grown = await visibleScreen(page, restoredKey);
      return spuriousWraps(grown).length === 0 && historyNumbers(grown.rows).length > 0;
    }, { timeout: 20_000 })
    .toBe(true);
  const grown = await visibleScreen(page, restoredKey);
  expect(
    spuriousWraps(grown),
    `post-paint wider fit wrapped snapshot cells (cols ${colsBeforeGrow} -> ${grown.cols}):\n${grown.rows.join("\n")}`,
  ).toEqual([]);
});

test("syncout survives a reload without a window resize", async ({ page }) => {
  await logIn(page);
  const sessions = page.locator(".sb-session");
  await expect(sessions.nth(0)).toBeVisible();
  await sessions.nth(0).click();
  await expect(page.locator(".xterm")).toBeVisible();

  const textarea = page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
  await textarea.focus();
  await page.keyboard.type("syncout 200 0");
  await page.keyboard.press("Enter");

  const key = await visibleLayerKey(page);
  await expect
    .poll(async () => (await visibleScreen(page, key)).rows.some((row) => row.includes("CODEX-SYNC")), { timeout: 20_000 })
    .toBe(true);

  await page.reload();
  await expect(page.locator(".xterm")).toBeVisible();
  await sessions.nth(0).click();
  await expect(page.locator('.term-layer[style*="visible"] .xterm')).toBeVisible();

  const restoredKey = await visibleLayerKey(page);
  await expect
    .poll(async () => (await visibleScreen(page, restoredKey)).rows.some((row) => row.includes("CODEX-SYNC")), { timeout: 20_000 })
    .toBe(true);

  const screen = await visibleScreen(page, restoredKey);
  expect(
    spuriousWraps(screen),
    `syncout reload wrapped snapshot cells:\n${screen.rows.join("\n")}`,
  ).toEqual([]);
  const syncRows = screen.rows.filter((row) => /CODEX-SYNC-\d+/.test(row));
  const ids = syncRows.map((row) => Number(/CODEX-SYNC-(\d+)/.exec(row)?.[1]));
  expect(ids.filter((n, i) => ids.indexOf(n) !== i), `syncout reload duplicated lines:\n${screen.rows.join("\n")}`).toEqual([]);
});
