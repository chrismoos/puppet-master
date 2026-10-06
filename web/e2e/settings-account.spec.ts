import { expect, test } from "./fixtures";
import { logIn } from "./support";

test.use({ viewport: { width: 1440, height: 900 } });

test("the gear opens Settings, keeps the session mounted and Back returns to its exact route", async ({ page }) => {
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await expect(page.locator(".xterm")).toBeVisible();
  const sessionAddress = page.url();
  const terminal = await page.locator(".term-layer[style*='visibility: visible'] .xterm").elementHandle();
  await page.getByRole("link", { name: "Settings", exact: true }).click();
  await expect(page).toHaveURL(/#\/settings\/appearance$/);
  // The modal is gone: Settings is a page, with nothing layered over the app.
  await expect(page.getByRole("dialog")).toHaveCount(0);
  await expect(page.getByRole("heading", { level: 2, name: "Appearance" })).toBeFocused();

  const nav = page.getByRole("navigation", { name: "Settings" });
  await nav.getByRole("link", { name: "Password" }).click();
  await expect(page).toHaveURL(/#\/settings\/password$/);
  await expect(page.getByLabel("Current password", { exact: true })).toBeVisible();
  await expect(nav.getByRole("link", { name: "Password" })).toHaveAttribute("aria-current", "page");
  await nav.getByRole("link", { name: "Notifications" }).click();
  await expect(page).toHaveURL(/#\/settings\/notifications$/);
  await expect(page.getByText("Hold phone notifications while I work here")).toBeVisible();

  await nav.getByRole("button", { name: "Back to sessions" }).click();
  await expect(page).toHaveURL(sessionAddress);
  expect(await terminal!.evaluate((element) => element.isConnected)).toBe(true);
  await expect(page.locator(".xterm")).toBeVisible();

  // Each page is its own history entry, so browser Back walks the pages.
  await page.goBack();
  await expect(page).toHaveURL(/#\/settings\/notifications$/);
  await page.goBack();
  await expect(page).toHaveURL(/#\/settings\/password$/);
});

test("background board updates keep the settings address and Back returns to the board", async ({ page }) => {
  await page.clock.install();
  await logIn(page);
  const boardAddress = `${process.env.PM_E2E_BASE_URL!}/#/bucket/1/board`;
  await page.goto(boardAddress);
  await expect(page.locator(".workbench")).toBeVisible();
  await page.getByRole("link", { name: "Settings", exact: true }).click();
  await expect(page.getByRole("heading", { level: 2, name: "Appearance" })).toBeVisible();
  await page.clock.runFor(DASHBOARD_ACTIVITY_WINDOW_MS);
  await expect(page).toHaveURL(/#\/settings\/appearance$/);
  await page.getByRole("button", { name: "Back to sessions" }).click();
  await expect(page).toHaveURL(boardAddress);
  await expect(page.locator(".workbench")).toBeVisible();
});

const DASHBOARD_ACTIVITY_WINDOW_MS = 1000;

const pages = [
  { path: "appearance", title: "Appearance" },
  { path: "terminal-theme", title: "Terminal theme" },
  { path: "notifications", title: "Notifications" },
  { path: "password", title: "Password" },
];
for (const entry of pages) {
  test(`direct ${entry.path} address displays only its page after reload`, async ({ page }) => {
    await logIn(page);
    await page.goto(`${process.env.PM_E2E_BASE_URL!}/#/settings/${entry.path}`);
    await page.reload();
    await expect(page.getByRole("heading", { level: 2 })).toHaveText([entry.title]);
    await expect(page.getByRole("navigation", { name: "Settings" }).getByRole("link", { name: entry.title }))
      .toHaveAttribute("aria-current", "page");
  });
}

test("Appearance offers System, Light and Dark, in step with the top-bar toggle", async ({ page }) => {
  await logIn(page);
  await page.goto(`${process.env.PM_E2E_BASE_URL!}/#/settings/appearance`);
  const mode = page.getByRole("group", { name: "Mode" });
  const toggle = page.locator(".appearance-toggle");
  await expect(mode.getByRole("button")).toHaveText(["System", "Light", "Dark"]);
  await expect(mode.getByRole("button", { name: "System" })).toHaveAttribute("aria-pressed", "true");

  await mode.getByRole("button", { name: "Dark" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-appearance", "dark");
  await expect(mode.getByRole("button", { name: "Dark" })).toHaveAttribute("aria-pressed", "true");
  await expect(toggle).toHaveAttribute("data-appearance", "dark");

  await mode.getByRole("button", { name: "Light" }).click();
  await expect(page.locator("html")).toHaveAttribute("data-appearance", "light");
  await expect(toggle).toHaveAttribute("data-appearance", "light");

  // The toggle and the control are two views of one setting.
  await toggle.click();
  await expect(mode.getByRole("button", { name: "Dark" })).toHaveAttribute("aria-pressed", "true");
  await toggle.click();
  await expect(mode.getByRole("button", { name: "System" })).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator("html")).not.toHaveAttribute("data-appearance");

  // The mode control sits above the three theme cards.
  const cards = page.getByRole("list", { name: "Application theme" });
  await expect(cards.getByRole("button")).toHaveCount(3);
  expect((await mode.boundingBox())!.y).toBeLessThan((await cards.boundingBox())!.y);
});

test("Notifications is one row whose minutes stepper saves as it changes", async ({ page }) => {
  await logIn(page);
  await page.goto(`${process.env.PM_E2E_BASE_URL!}/#/settings/notifications`);
  await expect(page.locator(".set-page .ui-row")).toHaveCount(1);
  await expect(page.getByRole("button", { name: "Save" })).toHaveCount(0);
  const minutes = page.getByRole("textbox", { name: "Idle minutes" });
  await expect(minutes).toHaveValue("3");

  await page.getByRole("button", { name: "Increase", exact: true }).click();
  await expect(page.getByRole("status")).toHaveText("Notifications wait until you have been idle here for 4 minutes.");
  await page.getByRole("button", { name: "Decrease", exact: true }).click();
  await expect(page.getByRole("status")).toHaveText("Notifications wait until you have been idle here for 3 minutes.");

  await minutes.fill("soon");
  await expect(page.getByRole("alert")).toHaveText("Enter a whole number of minutes between 0 and 1440.");
  await minutes.fill("0");
  await minutes.press("Enter");
  await expect(page.getByRole("status")).toHaveText("Your phone is notified even while you are working here.");
  await page.reload();
  await expect(page.getByRole("textbox", { name: "Idle minutes" })).toHaveValue("0");
  await page.getByRole("button", { name: "Increase", exact: true }).click();
  await page.getByRole("button", { name: "Increase", exact: true }).click();
  await page.getByRole("button", { name: "Increase", exact: true }).click();
  await expect(page.getByRole("textbox", { name: "Idle minutes" })).toHaveValue("3");
});

const PASSWORD_FIELD_WIDTH_PX = 320;

test("Password uses password-width fields and validates before sending", async ({ page }) => {
  await logIn(page);
  await page.goto(`${process.env.PM_E2E_BASE_URL!}/#/settings/password`);
  const fields = ["Current password", "New password", "Confirm new password"];
  for (const label of fields) {
    const field = page.getByLabel(label, { exact: true });
    await expect(field).toHaveAttribute("type", "password");
    expect((await field.boundingBox())!.width).toBe(PASSWORD_FIELD_WIDTH_PX);
  }
  await expect(page.getByText("At least 8 characters.")).toBeVisible();

  await page.getByLabel("Current password", { exact: true }).fill("unfinished");
  await page.getByRole("button", { name: "Change password" }).click();
  await expect(page.getByRole("alert")).toHaveText("Fill in every field.");
  await page.getByLabel("New password", { exact: true }).fill("short");
  await page.getByLabel("Confirm new password", { exact: true }).fill("short");
  await page.getByRole("button", { name: "Change password" }).click();
  await expect(page.getByRole("alert")).toHaveText("The new password must be at least 8 characters.");
});
