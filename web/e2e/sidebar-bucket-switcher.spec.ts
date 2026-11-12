import { execFileSync } from "node:child_process";
import { expect, test, type Locator, type Page } from "./fixtures";
import { logIn } from "./support";

const SECOND_BUCKET = "second-switcher-bucket";

function cli(args: string[]): string {
  return execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
}

function option(page: Page, name: string | RegExp): Locator {
  return page.locator(".sb-switcher-list").getByRole("option", { name });
}

async function pressSlashOutsideInputs(page: Page): Promise<void> {
  await page.evaluate(() => (document.activeElement as HTMLElement | null)?.blur());
  await page.keyboard.press("/");
}

test("the switcher scopes the sidebar to one bucket and follows the selection", async ({ page, isolatedDaemon }) => {
  cli(["bucket", "add", SECOND_BUCKET]);
  await page.addInitScript(() => {
    if (localStorage.getItem("pm.showEnded") === null) localStorage.setItem("pm.showEnded", "false");
  });
  await logIn(page);

  const trigger = page.locator(".sb-switcher-trigger");
  const buckets = page.locator(".sb-scroll > .sb-bucket");
  const rows = page.locator(".sb-session");
  await expect(trigger).toHaveText(/^All buckets/);
  await expect(buckets).toHaveCount(2);
  await expect(rows).toHaveCount(2);

  await trigger.click();
  await expect(page.getByRole("combobox", { name: "Find a bucket or session" })).toBeFocused();
  await option(page, new RegExp(`^${SECOND_BUCKET}`)).click();
  await expect(page.locator(".sb-switcher-menu")).toHaveCount(0);
  await expect(trigger).toHaveText(new RegExp(`^${SECOND_BUCKET}`));
  await expect(buckets).toHaveCount(1);
  await expect(rows).toHaveCount(0);
  await expect(page.locator(".sb-bucket-head")).toHaveText(SECOND_BUCKET);
  await expect(page.getByText("no sessions in this bucket yet")).toBeVisible();
  await expect(page.locator(`[title="new session in ${SECOND_BUCKET}"]`)).toBeVisible();

  isolatedDaemon.hookSession("browser-e2e", "needs-input", "Pick a merge order");
  await expect(page.locator(".sb-switcher-elsewhere")).toHaveText("+1");

  await page.reload();
  await expect(trigger).toHaveText(new RegExp(`^${SECOND_BUCKET}`));
  await expect(rows).toHaveCount(0);

  await pressSlashOutsideInputs(page);
  const finder = page.getByRole("combobox", { name: "Find a bucket or session" });
  await expect(finder).toBeFocused();
  await finder.pressSequentially("browser");
  await expect(option(page, /^browser-e2e/).first()).toHaveAttribute("aria-selected", "true");
  await finder.press("Enter");
  await expect(trigger).toHaveText(/^browser-e2e/);
  await expect(rows).toHaveCount(2);
  await expect(page.locator(".sb-switcher-elsewhere")).toHaveCount(0);

  await trigger.click();
  await option(page, /^Needs you/).click();
  await expect(trigger).toHaveText(/^Needs you/);
  await expect(rows).toHaveCount(1);
  await expect(rows.first()).toContainText("browser-e2e");

  await trigger.click();
  await option(page, new RegExp(`^${SECOND_BUCKET}`)).click();
  await expect(rows).toHaveCount(0);
  const other = isolatedDaemon.session("browser-e2e-two");
  if (!other) throw new Error("browser-e2e-two was not seeded");
  await page.evaluate((id) => { location.hash = `#/session/${id}`; }, other.id);
  await expect(trigger).toHaveText(/^browser-e2e/);
  await expect(page.locator(".sb-session.is-selected")).toContainText("browser-e2e-two");
});

test("switcher search finds sessions in any bucket without moving the list", async ({ page, isolatedDaemon }) => {
  await page.addInitScript(() => localStorage.setItem("pm.showEnded", "false"));
  await logIn(page);

  const sidebar = page.locator(".sidebar");
  const trigger = page.locator(".sb-switcher-trigger");
  const rows = page.locator(".sb-session");
  const firstBucketTop = () => page.locator(".sb-scroll > .sb-bucket").first()
    .evaluate((element) => element.getBoundingClientRect().top);

  for (const width of [220, 304, 620]) {
    await page.locator(".workspace").evaluate((element, value) => {
      (element as HTMLElement).style.setProperty("--sidebar-w", `${value}px`);
    }, width);
    const overflow = await sidebar.evaluate((element) => element.scrollWidth - element.clientWidth);
    expect(overflow).toBeLessThanOrEqual(0);
  }

  const closedTop = await firstBucketTop();
  await trigger.click();
  const columnEdges = await page.locator(".sb-switcher-item.has-columns").evaluateAll((items) => items.map((item) => {
    const needs = item.querySelector(".sb-switcher-needs-cell")!.getBoundingClientRect();
    const count = item.querySelector(".sb-switcher-count")!.getBoundingClientRect();
    return [needs.left, needs.right, count.left, count.right].map(Math.round).join(",");
  }));
  expect(columnEdges.length).toBeGreaterThan(2);
  expect(new Set(columnEdges).size).toBe(1);
  const finder = page.getByRole("combobox", { name: "Find a bucket or session" });
  await finder.fill("browser-e2e");
  await expect(option(page, "Show all 2 results")).toBeVisible();
  await expect(page.locator(".sb-switcher-label")).toHaveText(["Buckets", "Sessions"]);
  await expect(option(page, /^browser-e2e-two/)).toBeVisible();
  expect(await firstBucketTop()).toBeCloseTo(closedTop, 0);
  expect(await sidebar.evaluate((element) => element.scrollWidth - element.clientWidth)).toBeLessThanOrEqual(0);

  await option(page, "Show all 2 results").click();
  const results = page.locator(".sb-results-bar");
  await expect(results).toContainText("2 results for “browser-e2e”");
  await expect(rows).toHaveCount(2);
  await results.getByRole("button", { name: "clear" }).click();
  await expect(results).toHaveCount(0);

  await trigger.click();
  await finder.fill("no-session-answers-to-this");
  await expect(page.locator(".sb-switcher-status")).toHaveText("no matches");
  await finder.press("Escape");
  await expect(page.locator(".sb-switcher-menu")).toHaveCount(0);
  await trigger.click();
  await expect(finder).toHaveValue("");
  await page.mouse.click(5, 5);
  await expect(page.locator(".sb-switcher-menu")).toHaveCount(0);

  await trigger.click();
  await finder.fill("browser-e2e-two");
  await option(page, /^browser-e2e-two/).click();
  await expect(page.locator(".sb-session.is-selected")).toContainText("browser-e2e-two");
  await expect(trigger).toHaveText(/^All buckets/);

  isolatedDaemon.seedEndedSessions(40);
  await isolatedDaemon.restart();
  await page.reload();
  await page.getByLabel("show ended").check();
  await expect(rows).toHaveCount(42);
  const scroll = page.locator(".sb-scroll");
  const metrics = await scroll.evaluate((element) => ({
    clientHeight: element.clientHeight,
    scrollHeight: element.scrollHeight,
  }));
  expect(metrics.scrollHeight).toBeGreaterThan(metrics.clientHeight);

  await page.setViewportSize({ width: 720, height: 500 });
  const responsive = await sidebar.evaluate((element) => ({
    clientWidth: element.clientWidth,
    scrollWidth: element.scrollWidth,
  }));
  expect(responsive.scrollWidth).toBeLessThanOrEqual(responsive.clientWidth);
});

test("the rail style scopes the sidebar from a column of distinct bucket buttons", async ({ page, isolatedDaemon }) => {
  cli(["bucket", "add", SECOND_BUCKET]);
  cli(["bucket", "add", "bravo-edge"]);
  await page.addInitScript(() => localStorage.setItem("pm.showEnded", "false"));
  await logIn(page);

  await page.evaluate(() => { location.hash = "#/settings/appearance"; });
  const style = page.getByRole("group", { name: "Sidebar buckets" });
  await expect(style.getByRole("button", { name: "Dropdown" })).toHaveAttribute("aria-pressed", "true");
  await style.getByRole("button", { name: "Rail" }).click();
  await expect(style.getByRole("button", { name: "Rail" })).toHaveAttribute("aria-pressed", "true");
  await page.evaluate(() => { location.hash = "#/"; });

  const rail = page.getByRole("navigation", { name: "buckets" });
  await expect(page.locator(".sb-switcher-trigger")).toHaveCount(0);
  await expect(rail.locator(".sb-rail-btn")).toHaveCount(6);
  await expect(rail.getByRole("button", { name: "All buckets" })).toHaveAttribute("aria-pressed", "true");
  const glyph = (name: string) => rail.getByRole("button", { name, exact: true }).locator(".sb-rail-glyph");
  await expect(glyph("browser-e2e")).toHaveText("BE");
  await expect(glyph(SECOND_BUCKET)).toHaveText("SS");
  await expect(glyph("bravo-edge")).toHaveText("BR");

  await rail.getByRole("button", { name: SECOND_BUCKET, exact: true }).click();
  await expect(rail.getByRole("button", { name: SECOND_BUCKET, exact: true })).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator(".sb-session")).toHaveCount(0);

  isolatedDaemon.hookSession("browser-e2e", "needs-input", "Pick a merge order");
  await expect(rail.getByRole("button", { name: "browser-e2e, 1 need input" })).toBeVisible();
  await expect(rail.getByRole("button", { name: "Needs you, 1 need input" })).toBeVisible();
  const badgeOverhang = await rail.evaluate((element) => {
    const edge = element.getBoundingClientRect().right;
    return Math.max(...[...element.querySelectorAll(".sb-rail-badge")].map((badge) => badge.getBoundingClientRect().right - edge));
  });
  expect(badgeOverhang).toBeLessThanOrEqual(0);

  const search = rail.getByRole("button", { name: "Search sessions" });
  const finder = page.getByRole("combobox", { name: "Find a bucket or session" });
  const rowsTop = () => page.locator(".sb-scroll").evaluate((element) => element.getBoundingClientRect().top);
  const closedTop = await rowsTop();
  await search.click();
  await expect(finder).toBeFocused();
  await expect(search).toHaveAttribute("aria-expanded", "true");
  expect(await rowsTop()).toBeCloseTo(closedTop, 0);
  await finder.fill("browser-e2e-two");
  await search.click();
  await expect(page.locator(".sb-switcher-menu")).toHaveCount(0);
  await search.click();
  await expect(finder).toHaveValue("");
  await finder.fill("browser-e2e-two");
  await page.locator(".sb-switcher-list").getByRole("option", { name: /^browser-e2e-two/ }).click();
  await expect(page.locator(".sb-switcher-menu")).toHaveCount(0);
  await expect(rail.getByRole("button", { name: /^browser-e2e/ }).first()).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator(".sb-session.is-selected")).toContainText("browser-e2e-two");

  await page.reload();
  await expect(rail.getByRole("button", { name: /^browser-e2e/ }).first()).toHaveAttribute("aria-pressed", "true");
  // The selected session's terminal takes focus once it attaches, and a slash typed there belongs to it.
  await expect(async () => {
    if (await page.locator(".sb-switcher-menu").count()) await page.keyboard.press("Escape");
    await rail.getByRole("button", { name: "All buckets" }).focus();
    await page.keyboard.press("/");
    await expect(finder).toBeFocused({ timeout: 1_000 });
  }).toPass();
  await finder.press("Escape");
  await expect(page.locator(".sb-switcher-menu")).toHaveCount(0);
});

test("a bucket's own letters, color and picture replace its rail letters", async ({ page }) => {
  cli(["bucket", "add", SECOND_BUCKET]);
  await logIn(page);
  await page.evaluate(() => { location.hash = "#/settings/appearance"; });
  await page.getByRole("group", { name: "Sidebar buckets" }).getByRole("button", { name: "Rail" }).click();
  await page.evaluate(() => { location.hash = "#/"; });

  const rail = page.getByRole("navigation", { name: "buckets" });
  const button = rail.getByRole("button", { name: SECOND_BUCKET, exact: true });
  const openSettings = async () => {
    await button.click();
    await page.locator(`.sb-bucket-row [title="actions for ${SECOND_BUCKET}"]`).click();
    await page.getByRole("menuitem", { name: "Settings" }).click();
    await expect(page.locator(".bucket-icon-editor")).toBeVisible();
  };
  const save = async () => {
    await page.getByRole("button", { name: "Save changes" }).click();
    await expect(page).toHaveURL(/catalog=buckets/);
    await expect(page.locator(".bucket-icon-editor")).toHaveCount(0);
    await page.evaluate(() => { location.hash = "#/"; });
  };

  await openSettings();
  const editor = page.locator(".bucket-icon-editor");
  await expect(editor.getByLabel("Letters or emoji")).toHaveAttribute("placeholder", "SS");
  await editor.getByLabel("Letters or emoji").fill("Q7");
  await editor.getByLabel("Rail icon color").fill("#3dd68c");
  await expect(editor.locator(".bucket-icon-preview .sb-rail-glyph")).toHaveText("Q7");
  await save();
  await expect(button.locator(".sb-rail-glyph")).toHaveText("Q7");
  await expect(button).toHaveClass(/has-color/);
  await expect(button).toHaveAttribute("style", /--rail-color:\s*#3dd68c/);

  await openSettings();
  await editor.locator('input[type="file"]').setInputFiles("public/brand/puppet-master-app-icon-256.png");
  await expect(editor.locator(".bucket-icon-preview img")).toHaveAttribute("src", /^data:image\/(webp|png);base64,/);
  await expect(editor.getByLabel("Letters or emoji")).toBeDisabled();
  await save();
  const picture = button.locator("img.sb-rail-image");
  await expect(picture).toHaveAttribute("src", /^data:image\/(webp|png);base64,/);
  const natural = await picture.evaluate((image: HTMLImageElement) => [image.naturalWidth, image.naturalHeight]);
  expect(natural).toEqual([68, 68]);

  await page.reload();
  await expect(button.locator("img.sb-rail-image")).toBeVisible();
  await openSettings();
  await editor.getByRole("button", { name: "remove" }).click();
  await editor.getByRole("button", { name: "use default" }).click();
  await editor.getByLabel("Letters or emoji").fill("");
  await save();
  await expect(button.locator(".sb-rail-glyph")).toHaveText("SS");
  await expect(button).not.toHaveClass(/has-color/);
});
