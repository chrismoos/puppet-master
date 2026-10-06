import { expect, test } from "./fixtures";
import { logIn } from "./support";

test("Board inspector copies its internal item reference with keyboard feedback", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await logIn(page);
  await page.locator(".sb-board-link").click();
  await expect(page.locator(".workbench")).toBeVisible();

  await page.locator(".workbench-row").first().click();
  const reference = page.locator(".item-reference");
  const link = reference.getByRole("link");
  const copy = reference.getByRole("button", { name: /Copy item reference/ });
  const expected = await link.textContent();
  expect(expected).toMatch(/^pm:item\/\d+\/\d+$/);

  await copy.focus();
  await expect(copy).toBeFocused();
  await copy.press("Enter");
  await expect(reference.getByRole("status")).toHaveText("Copied");
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(expected);

  const href = await link.getAttribute("href");
  await link.click();
  await expect.poll(() => page.url()).toContain(href!.slice(1));
  await expect(page.getByRole("button", { name: "spawn session" })).toBeVisible();

  await page.evaluate(() => {
    Object.defineProperty(navigator, "clipboard", {
      configurable: true,
      value: { writeText: () => Promise.reject(new Error("synthetic clipboard failure")) },
    });
  });
  await copy.click();
  await expect(reference.getByRole("status")).toHaveText("Copy failed");
});
