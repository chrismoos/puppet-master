import { expect, test, type Locator, type Page } from "./fixtures";
import { logIn } from "./support";

const USERNAME = "browser-e2e";
const PASSWORD = "browser-e2e-password";
const VIEWPORTS = [
  { width: 1440, height: 1000 },
  { width: 360, height: 800 },
] as const;

async function expectContained(locator: Locator, page: Page): Promise<void> {
  const bounds = await locator.evaluate((element) => {
    const rect = element.getBoundingClientRect();
    return {
      left: rect.left,
      right: rect.right,
      width: rect.width,
      viewportWidth: document.documentElement.clientWidth,
    };
  });

  expect(bounds.left).toBeGreaterThanOrEqual(0);
  expect(bounds.right).toBeLessThanOrEqual(bounds.viewportWidth);
  expect(bounds.width).toBeGreaterThan(0);
  expect(await page.locator("html").evaluate((element) => element.scrollWidth)).toBeLessThanOrEqual(bounds.viewportWidth);
}

async function expectShellBrand(page: Page): Promise<void> {
  const topbar = page.locator(".topbar");
  const brand = page.locator(".topbar-brand");
  const image = brand.locator(".product-brand-image.for-dark-bg");

  await expect(brand).toHaveAttribute("aria-label", "Home");
  await expect(image).toHaveAttribute("src", "/brand/puppet-master-mark-color-dark.svg");
  await expect(image).toHaveAttribute("alt", "");
  await expect(image).toHaveAttribute("aria-hidden", "true");
  await expect(image).toBeVisible();
  await expect(page.locator(".conn")).toBeVisible();
  await expect(page.getByRole("button", { name: "enter focus mode" })).toBeVisible();
  await expect(topbar.getByRole("link", { name: "Settings", exact: true })).toBeVisible();
  await expect(page.locator(".account-menu-trigger")).toBeVisible();

  const metrics = await image.evaluate((element: HTMLImageElement) => ({
    width: element.getBoundingClientRect().width,
    height: element.getBoundingClientRect().height,
    naturalWidth: element.naturalWidth,
    naturalHeight: element.naturalHeight,
  }));
  expect(metrics).toEqual({ width: 28, height: 28, naturalWidth: 256, naturalHeight: 256 });
  expect(await topbar.evaluate((element) => element.scrollWidth)).toBeLessThanOrEqual(
    await topbar.evaluate((element) => element.clientWidth),
  );
  await expectContained(topbar, page);
}

async function logOut(page: Page): Promise<void> {
  await page.locator(".account-menu-trigger").click();
  await page.getByRole("menuitem", { name: "Log out" }).click();
  await expect(page.getByRole("button", { name: "log in" })).toBeVisible();
}

async function expectLoginBrand(page: Page): Promise<void> {
  const card = page.locator(".auth-card");
  const image = card.locator(".product-brand-image.for-dark-bg");

  await expect(image).toHaveAttribute("src", "/brand/puppet-master-lockup-color-dark.svg");
  await expect(image).toHaveAttribute("alt", "Puppet Master");
  await expect(image).toBeVisible();
  await expect(card.getByText("log in to your daemon", { exact: true })).toBeVisible();
  await expect(card.getByLabel("username")).toBeVisible();
  await expect(card.getByLabel("password")).toBeVisible();
  await expect(card.getByRole("button", { name: "log in" })).toBeVisible();

  const metrics = await image.evaluate((element: HTMLImageElement) => ({
    width: element.getBoundingClientRect().width,
    ratio: element.getBoundingClientRect().width / element.getBoundingClientRect().height,
    naturalWidth: element.naturalWidth,
    naturalHeight: element.naturalHeight,
    cardBackground: getComputedStyle(element.closest(".auth-card")!).backgroundColor,
  }));
  expect(metrics.width).toBeGreaterThanOrEqual(180);
  expect(metrics.ratio).toBeCloseTo(820 / 256, 2);
  expect(metrics.naturalWidth).toBe(820);
  expect(metrics.naturalHeight).toBe(256);
  expect(metrics.cardBackground).toBe("rgb(16, 20, 28)");
  await expectContained(card, page);
}

async function submitLogin(page: Page): Promise<void> {
  await page.getByLabel("username").fill(USERNAME);
  await page.getByLabel("password").fill(PASSWORD);
  await page.getByRole("button", { name: "log in" }).click();
  await expect(page.locator(".topbar-brand")).toBeVisible();
}

test("approved identity stays legible and compact on login and application shell", async ({ page }) => {
  await logIn(page);
  await expect(page).toHaveTitle("Puppet Master");
  await expect(page.locator('link[rel="icon"][type="image/svg+xml"]')).toHaveAttribute(
    "href",
    "/brand/puppet-master-app-icon.svg",
  );
  await expect(page.locator('link[rel="manifest"]')).toHaveAttribute("href", "/manifest.webmanifest");

  for (const viewport of VIEWPORTS) {
    await page.setViewportSize(viewport);
    await expectShellBrand(page);
    await logOut(page);
    await expectLoginBrand(page);

    if (viewport !== VIEWPORTS.at(-1)) await submitLogin(page);
  }
});
