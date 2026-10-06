import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

test("terminal debug bar reports live layer, socket, and mode state", async ({ page }) => {
  await logIn(page);
  await page.evaluate(() => localStorage.setItem("pm.terminal.debug", "1"));
  await page.reload();
  await page.locator(".sb-session-title")
    .filter({ hasText: /^browser-e2e$/ })
    .locator("xpath=ancestor::button[contains(@class, 'sb-session')][1]")
    .click();
  await expect(page.locator('.term-layer[style*="visible"]')).toBeVisible();

  await expect(page.getByRole("button", { name: "debug" })).toHaveCount(0);

  const bar = page.getByRole("status", { name: "terminal diagnostics" });
  await expect(bar).toBeVisible();
  const initialHeight = (await bar.boundingBox())?.height;
  await expect(bar).toContainText("buf=normal");
  await expect(bar).toContainText("mouse=");
  await expect(bar).toContainText("sock=online");
  await expect(bar).toContainText("replay=");
  await expect(bar.getByRole("button", { name: "copy" })).toBeVisible();

  // The bar keeps tracking after output and survives a session switch.
  const textarea = page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
  await textarea.focus();
  await page.keyboard.type("lineout 200");
  await page.keyboard.press("Enter");
  await expect(bar).toContainText(/lines=\d{3}/, { timeout: 15_000 });

  await page.locator(".sb-session-title")
    .filter({ hasText: /^browser-e2e-two$/ })
    .locator("xpath=ancestor::button[contains(@class, 'sb-session')][1]")
    .click();
  await expect(page.getByRole("status", { name: "terminal diagnostics" })).toBeVisible();
  await expect(bar).toContainText("(hidden)");
  // The bar height must not vary with layer count, or its growth would
  // refit the terminal and repaint away captured agent state.
  expect((await bar.boundingBox())?.height).toBe(initialHeight);

  await page.evaluate(() => localStorage.removeItem("pm.terminal.debug"));
  await page.reload();
  await expect(page.getByRole("status", { name: "terminal diagnostics" })).toHaveCount(0);
});
