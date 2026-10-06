import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

const SCROLLBAR_IDLE_MS = 750;
const SCROLLBACK_BYTES = 60_000;

const BAR = ".xterm-scrollable-element > .scrollbar.vertical";

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
  await page.addInitScript(() => {
    localStorage.setItem("pm.viewTabs", JSON.stringify(["session:1"]));
  });
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await fillScrollback(page);

  const shell = page.locator(".terminal-scrollbar-shell").first();
  const box = (await page.locator(BAR).first().boundingBox())!;

  // The bar is faded and ignores the pointer until scrolling reveals it, so a
  // wheel is what makes the thumb reachable in the first place.
  await page.mouse.move(box.x - 200, box.y + 120);
  await page.mouse.wheel(0, -200);
  await expect(shell).toHaveClass(/is-terminal-scrollbar-active/);
  await expect.poll(() => barOpacity(page)).toBeGreaterThan(0.5);

  await page.mouse.move(box.x + box.width / 2, box.y + 120);
  await page.mouse.down();
  await page.mouse.move(box.x + box.width / 2, box.y + 220, { steps: 6 });

  // e2e-real-time-wait: the defect being guarded against is the idle timer retracting the bar out from under the pointer dragging it, so the drag must outlast that real timer.
  await page.waitForTimeout(SCROLLBAR_IDLE_MS + 400);
  await expect(shell).toHaveClass(/is-terminal-scrollbar-active/);
  expect(await barOpacity(page)).toBe(1);
  await expect(page.locator(BAR).first()).toHaveCSS("pointer-events", "auto");

  await page.mouse.up();
  // e2e-real-time-wait: the release restarts the same wall-clock idle timer.
  await page.waitForTimeout(SCROLLBAR_IDLE_MS + 250);
  await expect(shell).not.toHaveClass(/is-terminal-scrollbar-active/);
  await expect.poll(() => barOpacity(page)).toBe(0);
});

test("a press that releases outside the terminal still lets the scrollbar retract", async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem("pm.viewTabs", JSON.stringify(["session:1"]));
  });
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await fillScrollback(page);

  const shell = page.locator(".terminal-scrollbar-shell").first();
  const box = (await page.locator(BAR).first().boundingBox())!;

  await page.mouse.move(box.x - 200, box.y + 120);
  await page.mouse.wheel(0, -200);
  await expect(shell).toHaveClass(/is-terminal-scrollbar-active/);

  await page.mouse.move(box.x + box.width / 2, box.y + 120);
  await page.mouse.down();
  await page.mouse.move(10, 10, { steps: 6 });
  await page.mouse.up();

  // e2e-real-time-wait: a drag released away from the bar must not strand the reveal, which only the wall-clock idle timer can prove.
  await page.waitForTimeout(SCROLLBAR_IDLE_MS + 250);
  await expect(shell).not.toHaveClass(/is-terminal-scrollbar-active/);
});
