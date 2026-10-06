import { execFileSync } from "node:child_process";
import { expect, test, type Locator, type Page } from "./fixtures";
import { logIn } from "./support";

// The app shell's structural containers. Any of them that overflows without
// scrolling holds content the user has no way to reach.
const SHELL_CONTAINERS = [
  ".shell",
  ".workspace",
  ".main-pane",
  ".restorable-view",
  ".board-route-view",
  ".set-shell",
  ".set-nav",
  ".set-main",
  ".workbench",
  ".workbench-layout",
  ".workbench-index",
  ".workbench-inspector",
  ".inspector-grid",
  ".pane",
  ".workspace-layout",
].join(",");

const TOLERANCE_PX = 1;

function cli(args: string[]): string {
  return execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
}

async function clippedContainers(page: Page): Promise<Array<Record<string, unknown>>> {
  return page.evaluate(([selector, tolerance]) => {
    return [...document.querySelectorAll<HTMLElement>(selector as string)]
      .filter((element) => getComputedStyle(element).visibility !== "hidden")
      .filter((element) => element.scrollHeight > element.clientHeight + (tolerance as number))
      .filter((element) => !["auto", "scroll"].includes(getComputedStyle(element).overflowY))
      .map((element) => ({
        className: element.className,
        clientHeight: element.clientHeight,
        scrollHeight: element.scrollHeight,
      }));
  }, [SHELL_CONTAINERS, TOLERANCE_PX] as const);
}

/** The gap between a region's bottom edge and the bottom of the viewport. */
async function deadZoneBelow(region: Locator): Promise<number> {
  return region.evaluate((element) => window.innerHeight - element.getBoundingClientRect().bottom);
}

/** How far past the bottom of the viewport a target sits once everything that
 *  can scroll has been scrolled as far down as it goes. */
async function overhangAtFullScroll(target: Locator): Promise<number> {
  return target.evaluate((element) => {
    for (let node: HTMLElement | null = element; node; node = node.parentElement) {
      if (["auto", "scroll"].includes(getComputedStyle(node).overflowY)) node.scrollTop = node.scrollHeight;
    }
    const page = document.scrollingElement;
    if (page) page.scrollTop = page.scrollHeight;
    return element.getBoundingClientRect().bottom - window.innerHeight;
  });
}

async function expectViewReachable(
  page: Page,
  view: string,
  region: Locator,
  bottomContent?: Locator,
): Promise<void> {
  await expect(region, view).toBeVisible();
  expect(await clippedContainers(page), `${view}: clipped containers`).toEqual([]);
  expect(await deadZoneBelow(region), `${view}: dead zone below the region`)
    .toBeLessThanOrEqual(TOLERANCE_PX);
  if (bottomContent) {
    expect(await overhangAtFullScroll(bottomContent), `${view}: bottom of content off screen`)
      .toBeLessThanOrEqual(TOLERANCE_PX);
  }
}

async function expectEveryViewReachable(page: Page): Promise<void> {
  const base = process.env.PM_E2E_BASE_URL!;

  await page.goto(`${base}/#/settings/daemon`);
  const daemonSettings = page.locator(".set-setting");
  await expect(daemonSettings.first()).toBeAttached();
  await expectViewReachable(page, "settings/daemon", page.locator(".set-shell"), daemonSettings.last());

  await page.goto(`${base}/#/settings/projects`);
  await expectViewReachable(page, "settings/projects", page.locator(".set-shell"));

  await page.goto(`${base}/#/settings/workers`);
  await page.getByRole("button", { name: "Add worker" }).click();
  const addWorker = page.getByRole("dialog", { name: "Add a worker" });
  await expectViewReachable(page, "settings/workers", page.locator(".set-shell"), addWorker);

  await page.goto(`${base}/#/settings/terminal-theme`);
  const themeHint = page.locator(".set-term-side > .ui-hint");
  await expectViewReachable(page, "settings/terminal-theme", page.locator(".set-shell"), themeHint);

  await page.goto(`${base}/#/settings/instructions`);
  const history = page.getByRole("heading", { name: "History" });
  await expectViewReachable(page, "settings/instructions", page.locator(".set-shell"), history);

  await page.goto(`${base}/#/bucket/1/board`);
  const rows = page.locator(".workbench-row");
  await expect(rows.first()).toBeAttached();
  await expectViewReachable(page, "board", page.locator(".workbench"), rows.last());

  await page.goto(`${base}/#/settings/notifications`);
  const notificationsHint = page.locator(".set-page > .ui-hint");
  await expectViewReachable(page, "settings/notifications", page.locator(".set-shell"), notificationsHint);

  await page.goto(`${base}/#/settings/password`);
  const changePassword = page.getByRole("button", { name: "Change password" });
  await expectViewReachable(page, "settings/password", page.locator(".set-shell"), changePassword);

  await page.goto(base);
  await page.locator(".sb-session").first().click();
  await expectViewReachable(page, "session", page.locator(".main-pane > .restorable-view .pane"));

  await page.getByTitle("new workspace").click();
  await page.getByRole("button", { name: "create workspace" }).click();
  await expect(page.locator(".workspace-pane")).toHaveCount(1);
  await expectViewReachable(page, "workspace", page.locator(".workspace-layout"));
}

test.describe("small viewports", () => {
  test.beforeEach(async ({ page }) => {
    await logIn(page);
    for (let index = 0; index < 4; index += 1) {
      cli(["items", "add", "--bucket", "1", "--status", "planned", `small viewport item ${index}`]);
    }
  });

  test.describe("on a phone-sized screen", () => {
    test.use({
      viewport: { width: 390, height: 664 },
      deviceScaleFactor: 3,
      isMobile: true,
      hasTouch: true,
    });

    test("every view reaches the bottom of its content", async ({ page }) => {
      await expectEveryViewReachable(page);
    });
  });

  test.describe("in a small desktop window", () => {
    test.use({ viewport: { width: 800, height: 400 } });

    test("every view reaches the bottom of its content", async ({ page }) => {
      await expectEveryViewReachable(page);
    });
  });
});
