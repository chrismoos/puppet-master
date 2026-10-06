import { expect, test } from "./fixtures";
import { logIn, openSettings } from "./support";

test("Catalogs separates inventories and opens refresh-safe editors", async ({ page }) => {
  await logIn(page);
  await openSettings(page, "Projects");

  const catalogs = page.getByRole("tablist", { name: "Project management catalogs" });
  await expect(catalogs).toBeVisible();
  await expect(page.getByRole("tab", { name: /Projects/ })).toHaveAttribute("aria-selected", "true");
  await expect(page.locator(".manage-project").first()).toBeVisible();
  await expect(page.locator(".manage-project .btn-danger")).toHaveCount(0);

  await page.getByRole("tab", { name: /Buckets/ }).click();
  await expect(page).toHaveURL(/catalog=buckets/);
  await expect(page.locator(".manage-bucket").first()).toBeVisible();
  await expect(page.locator(".manage-project")).toHaveCount(0);

  const bucket = page.locator(".manage-bucket").first();
  const bucketId = await bucket.getAttribute("data-bucket-id");
  await bucket.locator(".catalog-name").click();
  await expect(page).toHaveURL(new RegExp(`bucket=${bucketId}$`));
  const drawer = page.locator(".catalog-drawer");
  await expect(drawer).toBeVisible();
  await expect(drawer.getByText("Edit bucket")).toBeVisible();

  await drawer.getByRole("button", { name: "Delete…" }).click();
  await expect(drawer.getByText("Delete permanently?")).toBeVisible();
  await drawer.getByRole("button", { name: "Keep" }).click();
  await expect(drawer.getByText("Delete permanently?")).toBeHidden();

  await page.reload();
  await expect(page.locator(`.manage-bucket.is-selected[data-bucket-id="${bucketId}"]`)).toBeFocused();
  await expect(page.locator(".catalog-drawer")).toBeVisible();

  await page.getByRole("tab", { name: /Projects/ }).click();
  const project = page.locator(".manage-project").first();
  const projectId = await project.getAttribute("data-project-id");
  await project.locator(".catalog-name").click();
  await expect(page).toHaveURL(new RegExp(`project=${projectId}$`));
  await expect(page.locator(".catalog-drawer").getByText("Edit project")).toBeVisible();
  await expect(page.locator(".catalog-drawer").getByLabel("Absolute path")).toBeVisible();
});

test("Catalog inventory becomes cards and editor becomes a phone sheet", async ({ page }) => {
  await logIn(page);
  await page.setViewportSize({ width: 390, height: 760 });
  await page.goto(`${process.env.PM_E2E_BASE_URL!}#/settings/projects?catalog=buckets`);

  const bucket = page.locator(".manage-bucket").first();
  await expect(bucket).toBeVisible();
  expect(await bucket.evaluate((element) => getComputedStyle(element).display)).toBe("grid");
  await bucket.locator(".catalog-name").click();

  const drawer = page.locator(".catalog-drawer");
  await expect(drawer).toBeVisible();
  const box = await drawer.boundingBox();
  expect(box?.x).toBe(0);
  expect(box?.width).toBe(390);
  await expect(page.getByRole("button", { name: "Close editor" })).toBeVisible();
  await page.getByRole("button", { name: "Close editor" }).click();
  await expect(drawer).toBeHidden();
  await expect(page.locator(".topbar")).toBeVisible();
  await expect(page.getByRole("button", { name: "enter focus mode" })).toBeVisible();
});
