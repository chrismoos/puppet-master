import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

test("sidebar selections route sessions without creating top-level tabs", async ({ page }) => {
  await page.addInitScript(() => {
    localStorage.setItem("pm.viewTabs", JSON.stringify(["session:1", "session:2"]));
  });
  await logIn(page);
  const rows = page.locator(".sb-session");
  const tabs = page.locator(".workspace-tab");

  await rows.nth(0).click();
  await expect(rows.nth(0)).toHaveClass(/is-selected/);
  const firstRoute = page.url();
  await expect(tabs).toHaveCount(0);
  await expect.poll(() => page.evaluate(() => localStorage.getItem("pm.viewTabs"))).toBeNull();

  await rows.nth(1).click();
  await expect(rows.nth(1)).toHaveClass(/is-selected/);
  const secondRoute = page.url();
  expect(secondRoute).not.toBe(firstRoute);
  await expect(tabs).toHaveCount(0);
  await expect(page.locator('.term-layer[style*="visible"] .xterm-helper-textarea')).toBeFocused();

  await page.goBack();
  await expect(page).toHaveURL(firstRoute);
  await expect(rows.nth(0)).toHaveClass(/is-selected/);
  await page.goForward();
  await expect(page).toHaveURL(secondRoute);
  await expect(rows.nth(1)).toHaveClass(/is-selected/);

  await page.reload();
  await expect(page).toHaveURL(secondRoute);
  await expect(rows.nth(1)).toHaveClass(/is-selected/);
  await expect(tabs).toHaveCount(0);
  await expect(rows).toHaveCount(2);

  await page.getByRole("link", { name: "Home" }).click();
  await expect(tabs).toHaveCount(0);
  await expect(page.getByText("select a session", { exact: true })).toBeVisible();
});
