import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

/// The daemon polls its release channel every six hours and a test host
/// is on whatever build it was built from, so the published version is
/// the one thing a spec cannot get from a real controller. Only the
/// three fields the notice reads are replaced, and the rest of the
/// daemon's answer is passed through unchanged.
async function publishNewerBuild(page: Page, latest: string): Promise<void> {
  await page.route("**/api/version", async (route) => {
    const response = await route.fetch();
    const body = (await response.json()) as Record<string, unknown>;
    await route.fulfill({
      response,
      json: {
        ...body,
        latestVersion: latest,
        updateAvailable: latest,
        latestCheckedAtUnixMs: Date.now() - 2 * 3_600_000,
      },
    });
  });
}

test("the stamp stays quiet while the controller runs the newest build", async ({ page }) => {
  await logIn(page);
  const stamp = page.locator(".build-stamp");
  await expect(stamp).toBeVisible();
  await expect(stamp).not.toHaveClass(/update-stamp/);
  await expect(page.locator(".update-glyph")).toHaveCount(0);
});

test("a published build turns the stamp into a dialog explaining the update", async ({ page }) => {
  await publishNewerBuild(page, "99.0.0");
  await logIn(page);

  const stamp = page.locator(".build-stamp.update-stamp");
  await expect(stamp).toBeVisible();
  await expect(page.locator(".update-glyph")).toBeVisible();
  // The rev leaves the label for the tooltip, so the topbar carries the
  // version alone.
  await expect(stamp).toHaveAttribute("title", /running .+ — 99\.0\.0 is published on/);

  await stamp.click();
  const dialog = page.getByRole("dialog", { name: "Update available" });
  await expect(dialog).toBeVisible();
  await expect(dialog).toContainText("Run the following on the controller to get the latest version:");
  await expect(dialog).toContainText("pm update");
  await expect(dialog).toContainText("After the update is fetched, restart your daemon.");
  await expect(dialog).toContainText("checked 2 h ago");

  await dialog.getByRole("button", { name: "Close" }).click();
  await expect(dialog).toBeHidden();
});

test("the About page carries the build, the channel and the same command", async ({ page }) => {
  await publishNewerBuild(page, "99.0.0");
  await logIn(page);
  await page.goto(`${process.env.PM_E2E_BASE_URL!}/#/settings/about`);

  const about = page.getByRole("region", { name: "About this controller" });
  await expect(about).toBeVisible();
  await expect(about).toContainText("update available");
  await expect(about).toContainText("99.0.0");
  await expect(about).toContainText("pm update");
  await expect(about.locator(".about-facts dd").first()).toContainText("·");
});
