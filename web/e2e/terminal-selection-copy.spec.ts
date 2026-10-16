import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

const SESSION_HOST = '.term-layer[style*="visible"]';
/** Wide enough that a padded row is unmistakable, short of the lane's width. */
const PADDING = " ".repeat(40);
const TOP = "PADDED-TOP";
const BOTTOM = "PADDED-BOTTOM";

interface StageProbe {
  visibleId: string;
  layers: Map<string, {
    term: {
      getSelection(): string;
      buffer: {
        active: {
          baseY: number;
          cursorY: number;
          getLine(y: number): { translateToString(trimRight: boolean): string } | undefined;
        };
      };
    };
  }>;
}

async function openTerminal(page: Page) {
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);
  const input = page.locator(`${SESSION_HOST} .xterm-helper-textarea`);
  await input.focus();
  return input;
}

async function cellGeometry(page: Page) {
  return page.locator(`${SESSION_HOST} .xterm`).evaluate((element) => {
    const screen = element.querySelector<HTMLElement>(".xterm-screen")!;
    const input = element.querySelector<HTMLTextAreaElement>(".xterm-helper-textarea")!;
    const screenRect = screen.getBoundingClientRect();
    const cursorRect = input.getBoundingClientRect();
    return {
      left: screenRect.left,
      cursorTop: cursorRect.top,
      cellWidth: cursorRect.width,
      cellHeight: cursorRect.height,
    };
  });
}

/** The row above the cursor, which is where an echoed marker lands. */
async function rowAboveCursor(page: Page): Promise<string> {
  return page.evaluate(() => {
    const stage = (window as unknown as { __pmStage: StageProbe }).__pmStage;
    const buffer = stage.layers.get(stage.visibleId)!.term.buffer.active;
    return buffer.getLine(buffer.baseY + buffer.cursorY - 1)?.translateToString(true) ?? "";
  });
}

/** The selection xterm reports beside the text the copy path wrote. */
async function selectionAndClipboard(page: Page): Promise<{ reported: string; copied: string }> {
  return page.evaluate(async () => {
    const stage = (window as unknown as { __pmStage: StageProbe }).__pmStage;
    return {
      reported: stage.layers.get(stage.visibleId)!.term.getSelection(),
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
 * text does not reach, so xterm reports those rows as content out to the
 * window's width. Copying several of them put one trailing space per
 * unfilled column on the clipboard, which re-wraps the paste.
 */
test("a selection copy drops the padding each row carries", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  const input = await openTerminal(page);
  await echoMarker(page, input, TOP);
  await echoMarker(page, input, BOTTOM);

  // Drag from the first marker's output row down into the second's, the
  // way a reader grabs a few lines of what an agent printed.
  await expect.poll(async () => {
    const geometry = await cellGeometry(page);
    await page.mouse.move(geometry.left + geometry.cellWidth * 4, geometry.cursorTop - geometry.cellHeight * 2.5);
    await page.mouse.down();
    await page.mouse.move(geometry.left + geometry.cellWidth * 30, geometry.cursorTop - geometry.cellHeight * 0.5, { steps: 8 });
    await page.mouse.up();
    return (await selectionAndClipboard(page)).copied;
  }, { message: "select-to-copy across two padded rows", timeout: 20_000 }).toContain(BOTTOM);

  const { reported, copied } = await selectionAndClipboard(page);
  // A selection xterm reported without padding would prove nothing.
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
  const geometry = await cellGeometry(page);
  const rowY = geometry.cursorTop - geometry.cellHeight * 0.5;
  await page.mouse.move(geometry.left + geometry.cellWidth * 20, rowY);
  await page.mouse.down();
  await page.mouse.move(geometry.left + geometry.cellWidth * 44, rowY, { steps: 6 });
  await page.mouse.up();

  await expect(page.locator(".terminal-clipboard-status")).toBeHidden();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe("KEPT");
});
