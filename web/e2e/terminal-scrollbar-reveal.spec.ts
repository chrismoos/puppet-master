import { expect, test, type Page } from "./fixtures";
import { installScrollbarClock, logIn, SCROLL_RENDER_MS } from "./support";

import { SCROLLBAR_IDLE_MS } from "../src/transientScrollbar";
const SCROLLBACK_BYTES = 60_000;
const DRAG_DISTANCE_PX = 100;
const DRAG_STEPS = 6;

const BAR = '.term-layer[style*="visibility: visible"] .xterm-scrollable-element > .scrollbar.vertical';

function barOpacity(page: Page): Promise<number> {
  return page.locator(BAR).first().evaluate((element) =>
    Number.parseFloat(getComputedStyle(element).opacity));
}

async function fillScrollback(page: Page): Promise<void> {
  const textarea = page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
  await textarea.focus();
  await page.keyboard.type(`bigout ${SCROLLBACK_BYTES}`);
  await page.keyboard.press("Enter");
  await expect.poll(() => page.locator(`${BAR} .slider`).first()
    .evaluate((element) => Number.parseFloat((element as HTMLElement).style.height)))
    .toBeGreaterThan(0);
}

test("the terminal scrollbar stays up for the whole of a thumb drag", async ({ page }) => {
  await installScrollbarClock(page);
  await page.addInitScript(() => {
    localStorage.setItem("pm.viewTabs", JSON.stringify(["session:1"]));
  });
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await fillScrollback(page);

  await page.clock.pauseAt(new Date());
  const shell = page.locator('.term-layer[style*="visibility: visible"].terminal-scrollbar-shell');
  const box = (await page.locator(BAR).first().boundingBox())!;

  await page.mouse.move(box.x - 200, box.y + 120);
  await page.mouse.wheel(0, -200);
  await page.clock.runFor(SCROLL_RENDER_MS);
  await expect(shell).toHaveClass(/is-terminal-scrollbar-active/);
  await expect.poll(() => barOpacity(page)).toBeGreaterThan(0.5);

  const thumb = (await page.locator(`${BAR} .slider`).boundingBox())!;
  await page.mouse.move(thumb.x + thumb.width / 2, thumb.y + thumb.height / 2);
  await expect.poll(() => barOpacity(page)).toBeGreaterThan(0.5);
  await page.mouse.down();
  await page.mouse.move(thumb.x + thumb.width / 2, thumb.y + thumb.height / 2 + DRAG_DISTANCE_PX, { steps: DRAG_STEPS });

  await page.clock.runFor(SCROLLBAR_IDLE_MS);
  await expect(shell).toHaveClass(/is-terminal-scrollbar-active/);
  expect(await barOpacity(page)).toBe(1);
  await expect(page.locator(BAR).first()).toHaveCSS("pointer-events", "auto");

  await page.mouse.up();
  await page.clock.runFor(SCROLLBAR_IDLE_MS);
  await expect(shell).not.toHaveClass(/is-terminal-scrollbar-active/);
  await expect.poll(() => barOpacity(page)).toBe(0);
});

test("a press that releases outside the terminal still lets the scrollbar retract", async ({ page }) => {
  await installScrollbarClock(page);
  await page.addInitScript(() => {
    localStorage.setItem("pm.viewTabs", JSON.stringify(["session:1"]));
  });
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await fillScrollback(page);

  await page.clock.pauseAt(new Date());
  const shell = page.locator('.term-layer[style*="visibility: visible"].terminal-scrollbar-shell');
  const box = (await page.locator(BAR).first().boundingBox())!;

  await page.mouse.move(box.x - 200, box.y + 120);
  await page.mouse.wheel(0, -200);
  await page.clock.runFor(SCROLL_RENDER_MS);
  await expect(shell).toHaveClass(/is-terminal-scrollbar-active/);

  const thumb = (await page.locator(`${BAR} .slider`).boundingBox())!;
  await page.mouse.move(thumb.x + thumb.width / 2, thumb.y + thumb.height / 2);
  await expect.poll(() => barOpacity(page)).toBeGreaterThan(0.5);
  await page.mouse.down();
  await page.mouse.move(10, 10, { steps: DRAG_STEPS });
  await page.mouse.up();

  await page.clock.runFor(SCROLLBAR_IDLE_MS);
  await expect(shell).not.toHaveClass(/is-terminal-scrollbar-active/);
});
