import { expect, test } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";

test.skip(true, "The workspace tab bar is hidden, so these flows cannot open a workspace.");

test("a second viewer's size is taken back by the first input here, never by focus alone", async ({ page, context }, testInfo) => {
  await page.setViewportSize({ width: 1400, height: 900 });
  await logIn(page);
  await page.locator(".sb-session-title").filter({ hasText: /^browser-e2e$/ }).locator("xpath=ancestor::button[contains(@class, 'sb-session')][1]").click();
  await expectTerminalRevealed(page, "s:");
  const firstBanner = page.locator(".terminal-size-prompt");
  await expect(firstBanner).toBeHidden();

  const other = await context.newPage();
  await other.setViewportSize({ width: 700, height: 600 });
  await other.goto(page.url());
  await expectTerminalRevealed(other, "s:");
  await expect(firstBanner).toBeVisible();
  const otherBanner = other.locator(".terminal-size-prompt");
  await expect(otherBanner).toBeHidden();
  await page.screenshot({ path: testInfo.outputPath("size-mismatch-banner.png") });
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  // e2e-real-time-wait: focus must send no resize after the viewer assertion debounce expires.
  await page.waitForTimeout(300);
  await expect(firstBanner).toBeVisible();
  await expect(otherBanner).toBeHidden();

  // Another viewer taking the terminal means the user went to it, so moving
  // the mouse here again is a return that takes the size back.
  await page.mouse.move(400, 400);
  await page.mouse.move(460, 440);
  await expect(firstBanner).toBeHidden();
  await expect(otherBanner).toBeVisible();

  // The buttons are clicked without pointer input, which would itself count
  // as coming back to that window.
  await otherBanner.getByRole("button", { name: "Dismiss", exact: true }).dispatchEvent("click");
  await expect(otherBanner).toBeHidden();
  await page.setViewportSize({ width: 1300, height: 850 });
  // e2e-real-time-wait: dismissal must remain quiet after the other viewer's resize and replay settle.
  await other.waitForTimeout(400);
  await expect(otherBanner).toBeHidden();

  await other.setViewportSize({ width: 750, height: 620 });
  await expect(firstBanner).toBeVisible();
  await firstBanner.getByRole("button", { name: "Update", exact: true }).dispatchEvent("click");
  await expect(firstBanner).toBeHidden();
  await expect(otherBanner).toBeVisible();
  await otherBanner.getByRole("button", { name: "Update", exact: true }).dispatchEvent("click");
  await expect(otherBanner).toBeHidden();
  await expect(firstBanner).toBeVisible();
  await other.close();
});

test("workspace terminals take the size back on the same return", async ({ page, context }) => {
  await page.setViewportSize({ width: 1400, height: 900 });
  await logIn(page);
  await page.locator(".sb-session-title").filter({ hasText: /^browser-e2e$/ }).locator("xpath=ancestor::button[contains(@class, 'sb-session')][1]").click();
  await expectTerminalRevealed(page, "s:");
  const sessionUrl = page.url();
  await page.getByTitle("new workspace").click();
  await page.getByRole("button", { name: "create workspace" }).click();
  await page.getByLabel("terminal shown in pane").first().selectOption({ label: "browser-e2e · agent" });
  const banner = page.locator(".workspace-pane .terminal-size-prompt");
  await expect(banner).toBeHidden();
  const other = await context.newPage();
  await other.setViewportSize({ width: 700, height: 600 });
  await other.goto(sessionUrl);
  await expectTerminalRevealed(other, "s:");
  await expect(banner).toBeVisible();
  await page.mouse.move(400, 400);
  await page.mouse.move(460, 440);
  await expect(banner).toBeHidden();
  await expect(other.locator(".terminal-size-prompt")).toBeVisible();
  await other.close();
});
