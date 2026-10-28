import { expect, test } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";

interface StageWindow extends Window {
  __webglRequests?: number;
  __pmStage?: {
    debugSnapshot(): Array<{ key: string; visible: boolean; screenText: string[] }>;
  };
}

test("switching terminals keeps each canvas-backed screen state independent", async ({ page }) => {
  await page.addInitScript(() => {
    const state = window as StageWindow;
    state.__webglRequests = 0;
    const getContext = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = function (this: HTMLCanvasElement, kind: string, ...args: unknown[]) {
      if (kind === "webgl" || kind === "webgl2" || kind === "experimental-webgl") {
        state.__webglRequests! += 1;
      }
      return getContext.call(this, kind as "2d", ...args as []);
    } as typeof HTMLCanvasElement.prototype.getContext;
  });
  await logIn(page);
  const sessions = page.locator(".sb-session");
  await expect(sessions.nth(1)).toBeVisible();

  await sessions.nth(0).click();
  await expect(page.locator(".ghostty-terminal")).toBeVisible();
  await expectTerminalRevealed(page, "s:");
  const firstKey = await page.evaluate(() =>
    (window as StageWindow).__pmStage!.debugSnapshot().find((layer) => layer.visible)!.key);
  await page.locator('.term-layer[style*="visible"] .ghostty-input').focus();
  await page.keyboard.type("echo CANVAS-SESSION-FIRST");
  await page.keyboard.press("Enter");
  await expect.poll(() => page.evaluate((key) =>
    (window as StageWindow).__pmStage!.debugSnapshot().find((layer) => layer.key === key)!.screenText.join("\n"), firstKey,
  )).toContain("CANVAS-SESSION-FIRST");

  await sessions.nth(1).click();
  await expect(sessions.nth(1)).toHaveClass(/is-selected/);
  await expect.poll(() => page.evaluate(() => {
    const layers = (window as StageWindow).__pmStage!.debugSnapshot();
    return layers.filter((layer) => layer.visible).length === 1 && layers.length >= 2;
  })).toBe(true);
  const secondKey = await page.evaluate(() =>
    (window as StageWindow).__pmStage!.debugSnapshot().find((layer) => layer.visible)!.key);
  await page.locator('.term-layer[style*="visible"] .ghostty-input').focus();
  await page.keyboard.type("echo CANVAS-SESSION-SECOND");
  await page.keyboard.press("Enter");
  await expect.poll(() => page.evaluate((key) =>
    (window as StageWindow).__pmStage!.debugSnapshot().find((layer) => layer.key === key)!.screenText.join("\n"), secondKey,
  )).toContain("CANVAS-SESSION-SECOND");

  await sessions.nth(0).click();
  await expect.poll(() => page.evaluate((key) =>
    (window as StageWindow).__pmStage!.debugSnapshot().find((layer) => layer.key === key)!.screenText.join("\n"), firstKey,
  )).toContain("CANVAS-SESSION-FIRST");
  expect(await page.evaluate(() => (window as StageWindow).__webglRequests)).toBe(0);
  await expect(page.locator(".ghostty-terminal canvas")).toHaveCount(2);
});
