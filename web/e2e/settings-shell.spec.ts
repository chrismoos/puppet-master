import { expect, test, type Page, type TestInfo } from "./fixtures";
import { logIn, openSettings } from "./support";

async function attachScreenshot(page: Page, testInfo: TestInfo, name: string): Promise<void> {
  const body = await page.screenshot({ fullPage: true });
  await testInfo.attach(name, {
    body,
    contentType: "image/png",
  });
  const evidenceDir = process.env.PM_E2E_EVIDENCE_DIR;
  if (evidenceDir) await page.screenshot({ path: `${evidenceDir}/${name}.png`, fullPage: true });
}

test("Settings restores the session from Back and the wordmark without recreating its view", async ({ page }, testInfo) => {
  await logIn(page);
  await page.setViewportSize({ width: 1440, height: 900 });
  await page.locator(".sb-scroll .sb-session").first().click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);
  await expect(page.locator(".term-layer:visible .xterm-screen")).toBeVisible();
  const sessionUrl = page.url();
  await page.evaluate(() => {
    (window as Window & { __manageSessionView?: Element }).__manageSessionView =
      document.querySelector(".restorable-view");
  });

  const gear = page.getByRole("link", { name: "Settings", exact: true });
  await gear.click();
  await expect(page).toHaveURL(/#\/settings\/appearance$/);
  await expect(page.locator(".set-shell")).toBeVisible();
  await expect(gear).toHaveAttribute("aria-current", "page");
  await expect(page.getByRole("heading", { level: 1, name: "Settings" })).toBeVisible();
  // One title per page, focused so a screen reader lands on the page it opened.
  await expect(page.getByRole("heading", { level: 2 })).toHaveText(["Appearance"]);
  await expect(page.getByRole("heading", { level: 2, name: "Appearance" })).toBeFocused();
  await expect(page.locator(".topbar")).toBeVisible();
  await expect(page.getByRole("button", { name: "enter focus mode" })).toBeVisible();
  await expect(page.locator(".sidebar-shell")).toBeHidden();
  await expect(page.locator(".sidebar-resizer")).toBeHidden();
  await expect(page.locator(".workspace-tabs:visible")).toHaveCount(0);
  await expect(page.locator(".term-layer:visible")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "spawn", exact: true })).toHaveCount(0);

  await page.getByRole("button", { name: "enter focus mode" }).click();
  await expect(page.locator(".shell")).toHaveClass(/is-focus-mode/);
  await expect(page.getByRole("button", { name: "exit focus mode" })).toBeVisible();
  const nav = page.getByRole("navigation", { name: "Settings" });
  await expect(nav).toBeHidden();
  expect((await page.locator(".set-main").boundingBox())?.x).toBe(0);
  await page.keyboard.press("Escape");
  await expect(page.locator(".shell")).not.toHaveClass(/is-focus-mode/);
  await expect(nav).toBeVisible();
  await attachScreenshot(page, testInfo, "settings-shell-desktop");

  await nav.getByRole("button", { name: "Back to sessions" }).click();
  await expect(page).toHaveURL(sessionUrl);
  expect(await page.evaluate(() =>
    (window as Window & { __manageSessionView?: Element }).__manageSessionView ===
      document.querySelector(".restorable-view"),
  )).toBe(true);

  await gear.click();
  await page.locator(".topbar-brand").click();
  await expect(page).toHaveURL(sessionUrl);
  await expect(page.getByRole("button", { name: "spawn", exact: true })).toHaveCount(0);

  // The account menu no longer offers Settings: the gear is the one way in.
  await page.getByRole("button", { name: "Account menu for browser-e2e" }).click();
  await expect(page.getByRole("menuitem")).toHaveText(["Log out"]);
  await page.keyboard.press("Escape");
  await gear.click();
  await expect(page).toHaveURL(/#\/settings\/appearance$/);
  await page.getByRole("button", { name: "Back to sessions" }).click();
  await expect(page).toHaveURL(sessionUrl);
  expect(await page.evaluate(() =>
    (window as Window & { __manageSessionView?: Element }).__manageSessionView ===
      document.querySelector(".restorable-view"),
  )).toBe(true);

  await page.getByTitle(/new session in/).first().click();
  await expect(page.locator(".spawn-pop")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(page.locator(".spawn-pop")).toBeHidden();
});

test("Settings entered from Home restores the previously open live session", async ({ page }) => {
  await logIn(page);
  await page.locator(".sb-scroll .sb-session").first().click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);
  const sessionUrl = page.url();

  await page.getByRole("link", { name: "Home" }).click();
  await expect(page.getByText("select a session", { exact: true })).toBeVisible();
  await page.getByRole("link", { name: "Settings", exact: true }).click();
  await expect(page).toHaveURL(/#\/settings\/appearance$/);
  await page.locator(".topbar-brand").click();
  await expect(page).toHaveURL(sessionUrl);
});

test("Settings restores a saved workspace and keeps page routes through refresh", async ({ page }) => {
  await logIn(page);
  await page.locator(".sb-scroll .sb-session").first().click();
  await page.getByTitle("new workspace").click();
  await page.locator(".modal").locator("input").fill("Settings restore workspace");
  await page.locator(".modal").getByRole("button", { name: "create workspace" }).click();
  await expect(page).toHaveURL(/#\/workspace\/\d+$/);
  const workspaceUrl = page.url();
  await page.evaluate(() => {
    (window as Window & { __manageWorkspaceView?: Element }).__manageWorkspaceView =
      document.querySelector(".restorable-view");
  });

  await openSettings(page, "Projects");
  await expect(page).toHaveURL(/#\/settings\/projects$/);
  await page.getByRole("button", { name: "Back to sessions" }).click();
  await expect(page).toHaveURL(workspaceUrl);
  expect(await page.evaluate(() =>
    (window as Window & { __manageWorkspaceView?: Element }).__manageWorkspaceView ===
      document.querySelector(".restorable-view"),
  )).toBe(true);

  await openSettings(page, "Instructions");
  await expect(page).toHaveURL(/#\/settings\/instructions$/);
  await page.reload();
  await expect(page).toHaveURL(/#\/settings\/instructions$/);
  await expect(page.getByRole("link", { name: "Instructions" })).toHaveAttribute("aria-current", "page");
  const panel = page.locator(".instructions-panel");
  await panel.getByRole("textbox", { name: "Instructions" }).fill("unsaved instructions");
  const target = panel.getByRole("group", { name: "Applies to" });
  await target.getByRole("button", { name: "Workers" }).click();
  const discard = page.getByRole("dialog", { name: "Discard unsaved changes?" });
  await expect(discard).toBeVisible();
  await discard.getByRole("button", { name: "Keep editing" }).click();
  await expect(target.getByRole("button", { name: "Everyone" })).toHaveAttribute("aria-pressed", "true");
  await target.getByRole("button", { name: "Workers" }).click();
  await discard.getByRole("button", { name: "Discard and load" }).click();
  await expect(target.getByRole("button", { name: "Workers" })).toHaveAttribute("aria-pressed", "true");
  await page.getByRole("button", { name: "Back to sessions" }).click();
  await expect(page).toHaveURL(workspaceUrl);

  await page.getByRole("button", { name: `Delete Settings restore workspace` }).click();
  await page.locator(".modal").getByRole("button", { name: "delete workspace" }).click();
});

test("Settings uses its accessible narrow page selector", async ({ page }, testInfo) => {
  await logIn(page);
  await page.setViewportSize({ width: 390, height: 760 });
  await page.goto(`${process.env.PM_E2E_BASE_URL!}#/settings/workers`);
  await expect(page).toHaveURL(/#\/settings\/workers$/);
  const topbar = page.locator(".topbar");
  await expect(topbar).toBeVisible();
  await expect(page.getByRole("button", { name: "enter focus mode" })).toBeVisible();
  expect(await topbar.evaluate((element) => element.scrollWidth)).toBeLessThanOrEqual(
    await topbar.evaluate((element) => element.clientWidth),
  );
  await expect(page.getByRole("navigation", { name: "Settings" })).toBeHidden();
  const selector = page.getByRole("combobox", { name: "Settings page" });
  await expect(selector).toBeVisible();
  await expect(selector).toHaveValue("workers");
  // Every page is reachable from the selector, under the same three groups.
  await expect(selector.locator("optgroup")).toHaveCount(3);
  await expect(selector.locator("option")).toHaveCount(11);
  await selector.selectOption("daemon");
  await expect(page).toHaveURL(/#\/settings\/daemon$/);
  await expect(page.getByRole("heading", { level: 2 })).toHaveText(["Daemon"]);
  expect(await page.evaluate(() => document.documentElement.scrollWidth - document.documentElement.clientWidth))
    .toBeLessThanOrEqual(0);

  await page.getByRole("button", { name: "enter focus mode" }).click();
  await expect(selector).toBeHidden();
  await expect(page.locator(".set-main")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(selector).toBeVisible();
  await attachScreenshot(page, testInfo, "settings-shell-narrow");

  await page.getByRole("button", { name: "Sessions" }).click();
  await expect(page).not.toHaveURL(/#\/settings/);
});

test("the nav groups every page as plain text links with counts", async ({ page }) => {
  await logIn(page);
  await page.goto(`${process.env.PM_E2E_BASE_URL!}#/settings/appearance`);
  const nav = page.getByRole("navigation", { name: "Settings" });
  await expect(nav.getByRole("group")).toHaveCount(3);
  await expect(nav.getByRole("group", { name: "Your account" }).getByRole("link"))
    .toHaveText(["Appearance", "Terminal theme", "Notifications", "Password"]);
  await expect(nav.getByRole("group", { name: "Workspace" }).getByRole("link"))
    .toHaveText([/^Projects\d+$/, "Connections", /^Model profiles\d+$/, "Instructions"]);
  await expect(nav.getByRole("group", { name: "Controller" }).getByRole("link"))
    .toHaveText([/^Workers\d+$/, "Devices", "Daemon"]);
  await expect(nav.locator("svg, img")).toHaveCount(0);
  await expect(nav).toContainText("Signed in as browser-e2e");

  for (const label of ["Terminal theme", "Notifications", "Password", "Connections", "Daemon"]) {
    await nav.getByRole("link", { name: label }).click();
    await expect(nav.getByRole("link", { name: label })).toHaveAttribute("aria-current", "page");
    await expect(nav.locator('[aria-current="page"]')).toHaveCount(1);
  }
});

const OLD_ADDRESSES = [
  ["#/manage", "#/settings/projects"],
  ["#/manage/projects", "#/settings/projects"],
  ["#/manage/connections", "#/settings/connections"],
  ["#/manage/models", "#/settings/models"],
  ["#/manage/instructions", "#/settings/instructions"],
  ["#/manage/workers", "#/settings/workers"],
  ["#/manage/mobile", "#/settings/mobile"],
  ["#/manage/daemon", "#/settings/daemon"],
  ["#/manage/projects?catalog=buckets", "#/settings/projects?catalog=buckets"],
  ["#/settings", "#/settings/appearance"],
];

test("old Manage and settings addresses land on the same page at its new address", async ({ page }) => {
  await logIn(page);
  const base = process.env.PM_E2E_BASE_URL!;
  for (const [old, canonical] of OLD_ADDRESSES) {
    await page.goto(`${base}/${old}`);
    await page.reload();
    await expect(page, old).toHaveURL(`${base}/${canonical}`);
    await expect(page.locator(".set-nav [aria-current='page']"), old).toHaveCount(1);
  }

  // A redirect replaces the old address, so Back leaves Settings rather
  // than bouncing off the address that redirected.
  await page.goto(base);
  await page.locator(".sb-scroll .sb-session").first().click();
  const sessionUrl = page.url();
  await page.evaluate(() => { window.location.hash = "#/manage/daemon"; });
  await expect(page).toHaveURL(`${base}/#/settings/daemon`);
  await expect(page.getByRole("heading", { level: 2, name: "Daemon" })).toBeVisible();
  await page.goBack();
  await expect(page).toHaveURL(sessionUrl);
});

test("bucket and project menus open refresh-safe targeted Settings routes", async ({ page }) => {
  await logIn(page);
  await page.locator(".sb-scroll .sb-session").first().click();

  const bucketRow = page.locator(".sb-bucket-row").first();
  const bucketMenu = bucketRow.getByRole("button", { name: /^actions for / });
  // Activate the live trigger directly. Session activity can replace the row
  // while Chromium is synthesizing Enter's follow-up click under parallel load,
  // which can immediately toggle the replacement popover closed.
  await bucketMenu.click();
  await expect(bucketMenu).toHaveAttribute("aria-expanded", "true");
  const bucketPopover = bucketRow.getByRole("menu");
  await expect(bucketPopover).toBeVisible();
  await expect(bucketPopover.getByRole("menuitem", { name: "Settings", exact: true })).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(bucketPopover).toBeHidden();
  await expect(bucketMenu).toBeFocused();

  await bucketMenu.click();
  await expect(bucketMenu).toHaveAttribute("aria-expanded", "true");
  await page.keyboard.press("Tab");
  await expect(bucketPopover.getByRole("menuitem", { name: "Settings", exact: true })).toBeFocused();
  await page.keyboard.press("Enter");
  await expect(page).toHaveURL(/#\/settings\/projects\?bucket=\d+$/);
  const bucketTarget = page.locator(".manage-bucket.is-selected");
  await expect(bucketTarget).toHaveCount(1);
  await expect(bucketTarget).toBeFocused();
  const bucketId = await bucketTarget.getAttribute("data-bucket-id");
  expect(page.url()).toContain(`bucket=${bucketId}`);

  await page.reload();
  await expect(page.locator(`.manage-bucket.is-selected[data-bucket-id="${bucketId}"]`)).toBeFocused();
  await page.getByRole("button", { name: "Back to sessions" }).click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);

  // Project settings open from the Projects page, which the bucket Settings item reaches.
  await page.goto(`${process.env.PM_E2E_BASE_URL!}#/settings/projects?catalog=projects`);
  await page.locator(".manage-project").first().locator(".catalog-name").click();
  await expect(page).toHaveURL(/#\/settings\/projects\?bucket=\d+&project=\d+$/);

  const projectTarget = page.locator(".manage-project.is-selected");
  await expect(projectTarget).toHaveCount(1);
  await expect(projectTarget).toBeFocused();
  const projectId = await projectTarget.getAttribute("data-project-id");
  const parentBucketId = await projectTarget.getAttribute("data-bucket-id");
  expect(page.url()).toContain(`bucket=${parentBucketId}&project=${projectId}`);

  await page.reload();
  await expect(page.locator(`.manage-project.is-selected[data-project-id="${projectId}"]`)).toBeFocused();
  await page.getByRole("button", { name: "Back to sessions" }).click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);
});
