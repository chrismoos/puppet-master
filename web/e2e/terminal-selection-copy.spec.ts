import { expect, test, type Page } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";
import { terminalCanvasGeometry } from "./terminalCanvasDriver";

const SESSION_HOST = '.term-layer[style*="visible"]';
/** Wide enough that a padded row is unmistakable, short of the lane's width. */
const PADDING = " ".repeat(40);
const TOP = "PADDED-TOP";
const BOTTOM = "PADDED-BOTTOM";

interface StageProbe {
  debugSnapshot(): Array<{ key: string; visible: boolean }>;
  layers: Map<string, {
    term: {
      getSelection(): string;
      renderSnapshot(): { cursorY: number; rowData: Array<{ text: string; cells: Array<{ text: string; selected: boolean }> }> } | null;
    };
  }>;
}

async function openTerminal(page: Page) {
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);
  await expectTerminalRevealed(page, `s:${page.url().match(/#\/session\/(\d+)$/)![1]}`);
  const input = page.locator(`${SESSION_HOST} .ghostty-input`);
  await input.focus();
  return input;
}

async function markerGeometry(page: Page, marker: string) {
  const terminal = page.locator(`${SESSION_HOST} .ghostty-terminal`);
  const geometry = await terminalCanvasGeometry(terminal);
  const row = await page.evaluate((target) => {
    const stage = (window as unknown as { __pmStage: StageProbe }).__pmStage;
    const layer = stage.layers.get(stage.debugSnapshot().find((entry) => entry.visible)!.key)!;
    return layer.term.renderSnapshot()?.rowData.findIndex((candidate) => candidate.text.startsWith(`OUT ${target}`)) ?? -1;
  }, marker);
  expect(row, `rendered output row for ${marker}`).toBeGreaterThanOrEqual(0);
  return { ...geometry, y: geometry.top + geometry.cellHeight * (row + 0.5) };
}

/** The row above the cursor, which is where an echoed marker lands. */
async function rowAboveCursor(page: Page): Promise<string> {
  return page.evaluate(() => {
    const stage = (window as unknown as { __pmStage: StageProbe }).__pmStage;
    const layer = stage.layers.get(stage.debugSnapshot().find((entry) => entry.visible)!.key)!;
    const snapshot = layer.term.renderSnapshot();
    return snapshot?.rowData[snapshot.cursorY - 1]?.text ?? "";
  });
}

/** The selection adapter reports beside the text the copy path wrote. */
async function selectionAndClipboard(page: Page): Promise<{ reported: string; copied: string }> {
  return page.evaluate(async () => {
    const stage = (window as unknown as { __pmStage: StageProbe }).__pmStage;
    const key = stage.debugSnapshot().find((entry) => entry.visible)!.key;
    return {
      reported: (stage.layers.get(key)!.term.renderSnapshot()?.rowData ?? [])
        .filter((row) => row.cells.some((cell) => cell.selected))
        .map((row) => row.cells.filter((cell) => cell.selected).map((cell) => cell.text || " ").join(""))
        .join("\n"),
      copied: await navigator.clipboard.readText(),
    };
  });
}

async function echoMarker(page: Page, input: ReturnType<Page["locator"]>, marker: string): Promise<void> {
  await input.pressSequentially(`echo ${marker}${PADDING}`);
  await input.press("Enter");
  await expect.poll(() => rowAboveCursor(page), { message: `the ${marker} output row` })
    .toContain(`OUT ${marker}`);
}

/**
 * An agent that paints a background writes a space into every column its
 * text does not reach, so a multi-row copy must trim each row's blank tail.
 */
test("a selection copy drops the padding each row carries", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  const input = await openTerminal(page);
  await echoMarker(page, input, TOP);
  await echoMarker(page, input, BOTTOM);

  // Drag from the first marker's output row down into the second's, the
  // way a reader grabs a few lines of what an agent printed.
  await expect.poll(async () => {
    const geometry = await markerGeometry(page, TOP);
    const bottom = await markerGeometry(page, BOTTOM);
    await page.mouse.move(geometry.left + geometry.cellWidth * 4.5, geometry.y);
    await page.mouse.down();
    await page.mouse.move(bottom.left + bottom.cellWidth * 30.5, bottom.y, { steps: 8 });
    await page.mouse.up();
    return (await selectionAndClipboard(page)).copied;
  }, { message: "select-to-copy across two padded rows", timeout: 20_000 }).toContain(BOTTOM);

  await expect.poll(async () => (await selectionAndClipboard(page)).reported)
    .toMatch(/ {4,}(?:\n|$)/);
  const { reported, copied } = await selectionAndClipboard(page);
  // Confirm the selection crossed into cells the agent painted as padding.
  expect(reported.split("\n").filter((line) => / {4,}$/.test(line)).length).toBeGreaterThan(0);
  const lines = copied.split("\n");
  expect(lines.filter((line) => /[ \t]$/.test(line))).toEqual([]);
  expect(lines.at(0)).toContain(TOP);
  expect(lines.at(-1)).toContain(BOTTOM);
  expect(lines.length).toBeGreaterThan(1);
});

test("a selection of nothing but padding leaves the clipboard alone", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  const input = await openTerminal(page);
  await echoMarker(page, input, TOP);
  await page.evaluate(() => navigator.clipboard.writeText("KEPT"));

  // Drag across the padding to the right of the text, which holds spaces
  // and nothing else.
  const geometry = await markerGeometry(page, TOP);
  const rowY = geometry.y;
  await page.mouse.move(geometry.left + geometry.cellWidth * 20.5, rowY);
  await page.mouse.down();
  await page.mouse.move(geometry.left + geometry.cellWidth * 44.5, rowY, { steps: 6 });
  await page.mouse.up();

  await expect(page.locator(".terminal-clipboard-status")).toBeHidden();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe("KEPT");
});
