import { expect, test } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";
import { terminalCanvasGeometry } from "./terminalCanvasDriver";

type StageWindow = Window & {
  __pmStage?: {
    layers: Map<string, {
      el: HTMLElement;
      term: {
        cols: number;
        getSelection(): string;
        fontFamily: string;
      };
    }>;
    debugSnapshot(): Array<{ key: string; visible: boolean; cols: number; cellWidth: number | null; baseY: number }>;
  };
};

test("terminal uses the full host width with the scrollbar overlaying content", async ({ page }) => {
  await logIn(page);
  await page.locator(".sb-session-title")
    .filter({ hasText: /^browser-e2e$/ })
    .locator("xpath=ancestor::button[contains(@class, 'sb-session')][1]")
    .click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);
  await expectTerminalRevealed(page, `s:${page.url().match(/#\/session\/(\d+)$/)![1]}`);
  const layer = page.locator('.term-layer[style*="visible"]');
  await expect(layer).toBeVisible();
  await expect.poll(() => page.evaluate(() => {
    const snapshot = (window as StageWindow).__pmStage?.debugSnapshot().find((entry) => entry.visible);
    return Boolean(snapshot?.cellWidth && document.fonts.check('13px "JetBrains Mono Variable"'));
  })).toBe(true);

  const canvasGeometry = await terminalCanvasGeometry(layer.locator(".ghostty-terminal"));
  const geometry = await page.evaluate(() => {
    const stage = (window as StageWindow).__pmStage!;
    const snapshot = stage.debugSnapshot().find((entry) => entry.visible)!;
    for (const [key, candidate] of stage.layers) {
      if (key !== snapshot.key) continue;
      return {
        cols: snapshot.cols,
        cellWidth: snapshot.cellWidth,
        hostWidth: candidate.el.getBoundingClientRect().width,
        uiFontFamily: getComputedStyle(document.body).fontFamily,
        fontFamily: candidate.term.fontFamily,
        jetBrainsMonoLoaded: document.fonts.check('13px "JetBrains Mono Variable"'),
      };
    }
    throw new Error("visible layer missing");
  });
  expect(geometry.cellWidth).not.toBeNull();
  expect(geometry.jetBrainsMonoLoaded).toBe(true);
  expect(geometry.uiFontFamily).toContain("JetBrains Mono Variable");
  expect(geometry.fontFamily).toContain("JetBrains Mono Variable");
  const contentWidth = geometry.hostWidth - canvasGeometry.padding * 2;
  const fullWidthCols = Math.floor(contentWidth / geometry.cellWidth!);
  const gutteredCols = Math.floor((contentWidth - 14) / geometry.cellWidth!);
  expect(geometry.cols).toBe(fullWidthCols);
  expect(geometry.cols).toBeGreaterThan(gutteredCols);

  // Scrollback for scrollbar interaction.
  const textarea = layer.locator(".ghostty-input");
  await textarea.focus();
  await page.keyboard.type("lineout 400");
  await page.keyboard.press("Enter");
  await expect.poll(() => page.evaluate(() =>
    (window as StageWindow).__pmStage?.debugSnapshot().find((entry) => entry.visible)?.baseY ?? 0,
  )).toBeGreaterThan(300);

  // The scrollbar is hidden until scrolling and must not block the content
  // beneath it: select text by dragging within the strip it overlays.
  // Start the drag on the strip the scrollbar overlays (proving the faded
  // scrollbar does not intercept it) and sweep left into the row text.
  const screen = await layer.locator(".ghostty-canvas").boundingBox();
  const rightEdgeX = canvasGeometry.left + canvasGeometry.cellWidth * (geometry.cols - 0.5);
  const midY = screen!.y + screen!.height / 2;
  await page.mouse.move(rightEdgeX, midY);
  await page.mouse.down();
  await page.mouse.move(screen!.x + 40, midY, { steps: 6 });
  await page.mouse.up();
  const selection = await page.evaluate(() => {
    const stage = (window as StageWindow).__pmStage!;
    for (const [, candidate] of stage.layers) {
      if (candidate.el.style.visibility !== "visible") continue;
      return candidate.term.getSelection();
    }
    return "";
  });
  expect(selection.length).toBeGreaterThan(0);

  // Wheel reveals the overlay scrollbar and dragging it scrolls.
  await page.mouse.move(screen!.x + screen!.width / 2, midY);
  await page.mouse.wheel(0, -600);
  const slider = layer.locator(".ghostty-scrollbar-thumb");
  await expect(slider).toBeVisible();
  const before = await slider.evaluate((el) => Number.parseFloat((el as HTMLElement).style.top));
  const bounds = await slider.boundingBox();
  await page.mouse.move(bounds!.x + bounds!.width / 2, bounds!.y + bounds!.height / 2);
  await page.mouse.down();
  await page.mouse.move(bounds!.x + bounds!.width / 2, bounds!.y + bounds!.height / 2 - 60, { steps: 4 });
  await page.mouse.up();
  const after = await slider.evaluate((el) => Number.parseFloat((el as HTMLElement).style.top));
  expect(before - after).toBeGreaterThan(10);
});
