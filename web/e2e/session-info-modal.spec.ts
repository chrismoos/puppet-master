import { expect, test } from "./fixtures";
import { logIn } from "./support";

function seededRow(page: import("@playwright/test").Page) {
  return page.locator(".sb-session-row").filter({
    has: page.locator(".sb-session-title", { hasText: /^browser-e2e$/ }),
  }).first();
}

test("session Info is keyboard accessible, modal, contained, and focus restoring", async ({ page }) => {
  await logIn(page);
  const row = seededRow(page);
  const rowButton = row.locator(".sb-session");
  const menuButton = row.getByRole("button", { name: "actions for browser-e2e" });
  const initialUrl = page.url();
  const initialCurrent = await rowButton.getAttribute("aria-current");

  await expect(row.locator(".sb-launch-folder")).toHaveCount(0);
  await expect(row.locator(".sb-session-meta .worker-chip")).toHaveCount(0);

  await menuButton.focus();
  await menuButton.press("Enter");
  const menu = page.getByRole("menu");
  const info = menu.getByRole("menuitem", { name: "Info" });
  await expect(info).toBeVisible();
  await expect(menu.getByRole("menuitemradio", { name: "Worker" })).toBeVisible();
  await menuButton.press("Tab");
  await expect(info).toBeFocused();
  await info.press("Enter");

  const dialog = page.getByRole("dialog", { name: "browser-e2e" });
  await expect(dialog).toBeVisible();
  await expect(page.getByRole("menu")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Close session information" })).toBeFocused();
  await expect(dialog.getByText("Local", { exact: true })).toBeVisible();
  await expect(dialog.getByText("Configured project root.", { exact: true })).toBeVisible();
  await expect(dialog.getByText(/recorded when the session starts/)).toBeVisible();
  await expect(dialog.getByText(/not a live filesystem or git-status signal/)).toBeVisible();
  await expect(dialog.getByRole("button", { name: "Copy launch folder" })).toBeVisible();
  await expect(page).toHaveURL(initialUrl);
  expect(await rowButton.getAttribute("aria-current")).toBe(initialCurrent);

  await page.getByRole("button", { name: "Close session information" }).press("Shift+Tab");
  await expect(dialog.getByRole("button", { name: "Close", exact: true })).toBeFocused();

  await page.setViewportSize({ width: 360, height: 480 });
  const geometry = await dialog.evaluate((element) => {
    const rect = element.getBoundingClientRect();
    const scroll = element.querySelector<HTMLElement>(".session-info-scroll")!;
    const overflowing = [...element.querySelectorAll<HTMLElement>("*")]
      .filter((child) => child.scrollWidth > child.clientWidth + 1)
      .map((child) => child.className);
    return {
      left: rect.left,
      right: rect.right,
      top: rect.top,
      bottom: rect.bottom,
      viewportWidth: window.innerWidth,
      viewportHeight: window.innerHeight,
      scrollOverflowY: getComputedStyle(scroll).overflowY,
      overflowing,
    };
  });
  expect(geometry.left).toBeGreaterThanOrEqual(0);
  expect(geometry.right).toBeLessThanOrEqual(geometry.viewportWidth);
  expect(geometry.top).toBeGreaterThanOrEqual(0);
  expect(geometry.bottom).toBeLessThanOrEqual(geometry.viewportHeight);
  expect(geometry.scrollOverflowY).toBe("auto");
  expect(geometry.overflowing).toEqual([]);

  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(menuButton).toBeFocused();
  await expect(page).toHaveURL(initialUrl);

  await menuButton.click();
  await page.getByRole("menuitem", { name: "Info" }).click();
  await expect(page.getByRole("dialog", { name: "browser-e2e" })).toBeVisible();
  await page.getByRole("dialog").getByRole("button", { name: "Close", exact: true }).click();
  await expect(menuButton).toBeFocused();
  await expect(page).toHaveURL(initialUrl);
});
