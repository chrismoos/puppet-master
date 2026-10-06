import { expect, test } from "./fixtures";
import { logIn } from "./support";

test("a second viewer's size offers Update or Dismiss without reclaiming on focus", async ({ page, context }, testInfo) => {
  await page.setViewportSize({ width: 1400, height: 900 });
  await logIn(page);
  await page.locator(".sb-session-title").filter({ hasText: /^browser-e2e$/ }).locator("xpath=ancestor::button[contains(@class, 'sb-session')][1]").click();
  await expect(page.locator(".xterm")).toBeVisible();
  const firstBanner = page.locator(".terminal-size-prompt");
  await expect(firstBanner).toBeHidden();

  const other = await context.newPage();
  await other.setViewportSize({ width: 700, height: 600 });
  await other.goto(page.url());
  await expect(other.locator(".xterm")).toBeVisible();
  await expect(firstBanner).toBeVisible();
  await expect(other.locator(".terminal-size-prompt")).toBeHidden();
  await page.screenshot({ path: testInfo.outputPath("size-mismatch-banner.png") });
  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  // e2e-real-time-wait: focus must send no resize after the viewer assertion debounce expires.
  await page.waitForTimeout(300);
  await expect(firstBanner).toBeVisible();
  await expect(other.locator(".terminal-size-prompt")).toBeHidden();

  await firstBanner.getByRole("button", { name: "Dismiss", exact: true }).click();
  await expect(firstBanner).toBeHidden();
  await other.setViewportSize({ width: 750, height: 620 });
  await expect(other.locator(".terminal-size-prompt")).toBeHidden();
  // e2e-real-time-wait: dismissal must remain quiet after the other viewer's resize and replay settle.
  await page.waitForTimeout(400);
  await expect(firstBanner).toBeHidden();

  const otherBanner = other.locator(".terminal-size-prompt");
  await page.setViewportSize({ width: 1300, height: 850 });
  await expect(otherBanner).toBeVisible();
  await otherBanner.getByRole("button", { name: "Update", exact: true }).click();
  await expect(otherBanner).toBeHidden();
  await expect(firstBanner).toBeVisible();
  await firstBanner.getByRole("button", { name: "Update", exact: true }).click();
  await expect(firstBanner).toBeHidden();
  await expect(otherBanner).toBeVisible();
  await other.close();
});

test("workspace terminals offer the same explicit size choice", async ({ page, context }) => {
  await page.setViewportSize({ width: 1400, height: 900 });
  await logIn(page);
  await page.locator(".sb-session-title").filter({ hasText: /^browser-e2e$/ }).locator("xpath=ancestor::button[contains(@class, 'sb-session')][1]").click();
  await expect(page.locator(".xterm")).toBeVisible();
  const sessionUrl = page.url();
  await page.getByTitle("new workspace").click();
  await page.getByRole("button", { name: "create workspace" }).click();
  await page.getByLabel("terminal shown in pane").first().selectOption({ label: "browser-e2e · agent" });
  const banner = page.locator(".workspace-pane .terminal-size-prompt");
  await expect(banner).toBeHidden();
  const other = await context.newPage();
  await other.setViewportSize({ width: 700, height: 600 });
  await other.goto(sessionUrl);
  await expect(banner).toBeVisible();
  await banner.getByRole("button", { name: "Update", exact: true }).click();
  await expect(banner).toBeHidden();
  await expect(other.locator(".terminal-size-prompt")).toBeVisible();
  await other.close();
});
