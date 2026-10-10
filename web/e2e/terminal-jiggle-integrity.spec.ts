import { expect, test, type Page } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";

// The repaint kick shrinks the terminal by one row and grows it back. That is
// meant to make a full-screen program redraw. What it must not do is move the
// content already on screen: xterm reflows and scrolls on resize, and a row
// lost on the way down that is not restored on the way up leaves everything
// one line out — which is what "my input is above the input bar" looks like.

interface StageWindow extends Window {
  __pmStage?: {
    debugSnapshot(): Array<{ key: string; visible: boolean }>;
    layers: Map<string, {
      term: {
        cols: number;
        rows: number;
        resize(cols: number, rows: number): void;
        buffer: {
          active: {
            baseY: number;
            cursorY: number;
            length: number;
            getLine(row: number): { translateToString(trimRight?: boolean): string } | undefined;
          };
        };
      };
    }>;
  };
}

async function screen(page: Page): Promise<{ rows: string[]; cursorY: number; baseY: number }> {
  return page.evaluate(() => {
    const stage = (window as StageWindow).__pmStage;
    const key = stage?.debugSnapshot().find((layer) => layer.visible)?.key ?? "";
    const layer = stage?.layers.get(key);
    if (!layer) return { rows: [], cursorY: -1, baseY: -1 };
    const buffer = layer.term.buffer.active;
    const rows: string[] = [];
    for (let row = buffer.baseY; row < buffer.baseY + layer.term.rows; row += 1) {
      rows.push(buffer.getLine(row)?.translateToString(true) ?? "");
    }
    return { rows, cursorY: buffer.cursorY, baseY: buffer.baseY };
  });
}

async function jiggle(page: Page): Promise<void> {
  await page.evaluate(() => {
    const stage = (window as StageWindow).__pmStage;
    const key = stage?.debugSnapshot().find((layer) => layer.visible)?.key ?? "";
    const layer = stage?.layers.get(key);
    if (!layer) throw new Error("no visible layer");
    const { cols, rows } = layer.term;
    layer.term.resize(cols, rows - 1);
    layer.term.resize(cols, rows);
  });
}

test("the repaint kick leaves the rows already on screen where they were", async ({ page }) => {
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await expect(page.locator(".xterm")).toBeVisible();
  await expectTerminalRevealed(page, "s:");

  // Fill past the screen so the cursor sits at the bottom, which is where a
  // row lost to a shrink cannot come back on its own.
  const textarea = page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
  await textarea.focus();
  await page.keyboard.type("syncout 200 0");
  await page.keyboard.press("Enter");

  await expect
    .poll(async () => (await screen(page)).rows.filter((row) => row.trim()).length, { timeout: 20_000 })
    .toBeGreaterThan(10);

  const before = await screen(page);
  await jiggle(page);
  const after = await screen(page);

  expect(
    after.rows,
    `the repaint kick moved the screen\nbefore:\n${before.rows.join("\n")}\nafter:\n${after.rows.join("\n")}`,
  ).toEqual(before.rows);
});
