import { execFileSync } from "node:child_process";
import { expect, test, type Locator, type Page } from "./fixtures";
import { logIn } from "./support";

type SocketMetrics = { terminalCreated: number; terminalClosed: number };

function cli(args: string[]): string {
  return execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
}

async function terminalCommand(page: Page, command: string): Promise<void> {
  const textarea = page.locator(".term-layer:visible .xterm-helper-textarea");
  await expect(textarea).toBeAttached();
  await textarea.pressSequentially(command);
  await textarea.press("Enter");
}

async function expectFooterContained(footer: Locator): Promise<void> {
  const geometry = await footer.evaluate((element) => {
    const bounds = element.getBoundingClientRect();
    const children = [...element.children].map((child) => {
      const rect = child.getBoundingClientRect();
      return { left: rect.left, right: rect.right, top: rect.top, bottom: rect.bottom };
    });
    return {
      clientWidth: element.clientWidth,
      scrollWidth: element.scrollWidth,
      bounds: { left: bounds.left, right: bounds.right },
      children,
    };
  });
  expect(geometry.scrollWidth).toBeLessThanOrEqual(geometry.clientWidth);
  for (const child of geometry.children) {
    expect(child.left).toBeGreaterThanOrEqual(geometry.bounds.left);
    expect(child.right).toBeLessThanOrEqual(geometry.bounds.right + 1);
  }
  for (let index = 0; index < geometry.children.length; index += 1) {
    for (let other = index + 1; other < geometry.children.length; other += 1) {
      const a = geometry.children[index];
      const b = geometry.children[other];
      const overlaps = a.left < b.right && b.left < a.right && a.top < b.bottom && b.top < a.bottom;
      expect(overlaps).toBe(false);
    }
  }
}

test("Board replaces only the main pane and preserves Sidebar, terminal, and Board state", async ({ page }) => {
  await page.addInitScript(() => {
    const metrics: SocketMetrics = { terminalCreated: 0, terminalClosed: 0 };
    (window as Window & { __boardSidebarSockets: SocketMetrics }).__boardSidebarSockets = metrics;
    const NativeWebSocket = window.WebSocket;
    window.WebSocket = new Proxy(NativeWebSocket, {
      construct(target, args) {
        const socket = Reflect.construct(target, args) as WebSocket;
        if (String(args[0]).includes("/ws/terminal/")) {
          metrics.terminalCreated += 1;
          socket.addEventListener("close", () => metrics.terminalClosed += 1);
        }
        return socket;
      },
    }) as typeof WebSocket;
  });

  await logIn(page);
  await page.setViewportSize({ width: 1440, height: 720 });
  const sidebar = page.locator(".sidebar");
  await sidebar.evaluate((element) => element.setAttribute("data-mount-probe", "original"));
  const sidebarBox = await page.locator(".sidebar-shell").boundingBox();

  await page.locator(".sb-scroll .sb-session").first().click();
  await expect(page.locator(".term-layer:visible .xterm-screen")).toBeVisible();
  await terminalCommand(page, "echo retained-board-terminal");
  const terminal = page.locator(".term-layer:visible");
  await terminal.evaluate((element) => element.setAttribute("data-terminal-probe", "retained"));

  const boardControl = page.getByRole("button", { name: "open board" }).first();
  await boardControl.focus();
  await page.keyboard.press("Enter");
  await expect(page.locator(".workbench")).toBeVisible();
  await expect(sidebar).toHaveAttribute("data-mount-probe", "original");
  expect(await page.locator(".sidebar-shell").boundingBox()).toEqual(sidebarBox);
  await expect(page.locator(".sidebar-resizer")).toBeVisible();
  await expect(page.locator(".session-rail, .board-drawer-scrim, .sb-board-pin")).toHaveCount(0);
  await expect(page.locator(".sidebar-shell")).not.toHaveAttribute("role", "dialog");

  const activeBoard = page.getByRole("button", { name: /board, active/ });
  await expect(activeBoard).toHaveAttribute("aria-pressed", "true");
  await expect(activeBoard).toHaveClass(/is-selected/);
  expect(await activeBoard.evaluate((element) => getComputedStyle(element, "::before").content)).not.toBe("none");

  const search = page.getByRole("searchbox", { name: "search work items" });
  await search.fill("volume");
  const selectedRow = page.locator(".workbench-row").filter({
    has: page.locator("strong", { hasText: /^Volume item 42$/ }),
  });
  await expect(selectedRow).toHaveCount(1);
  await selectedRow.click();
  const indexResults = page.locator(".workbench-index-results");
  const indexScroll = await indexResults.evaluate((element) => element.scrollTop);
  expect(indexScroll).toBeGreaterThan(0);

  await page.locator(".sb-scroll .sb-session").first().click();
  await expect(page.locator('[data-terminal-probe="retained"]')).toBeVisible();
  await expect(page.locator(".workbench")).toBeHidden();
  await expect(sidebar).toHaveAttribute("data-mount-probe", "original");

  await page.getByRole("button", { name: "open board" }).first().click();
  await expect(search).toBeVisible();
  await expect(search).toHaveValue("volume");
  await expect(selectedRow).toHaveAttribute("aria-current", "true");
  expect(await indexResults.evaluate((element) => element.scrollTop)).toBe(indexScroll);

  await activeBoard.focus();
  await page.keyboard.press("Space");
  await expect(page.locator('[data-terminal-probe="retained"]')).toBeVisible();
  await expect(page.locator(".term-layer:visible .xterm-helper-textarea")).toBeFocused();
  expect(await page.evaluate(() =>
    (window as Window & { __boardSidebarSockets: SocketMetrics }).__boardSidebarSockets,
  )).toEqual({ terminalCreated: 1, terminalClosed: 0 });
});

test("Board state is bucket-local and deep links retain the ordinary Sidebar", async ({ page }) => {
  const secondBucket = cli(["bucket", "add", "second-board-bucket"]).match(/bucket (\d+) created/)?.[1];
  if (!secondBucket) throw new Error("could not create second Board bucket");
  await logIn(page);

  const buckets = page.locator(".sb-bucket");
  await expect(buckets).toHaveCount(2);
  const firstBoard = buckets.first().locator(".sb-board-link");
  const secondBoard = buckets.filter({ hasText: "second-board-bucket" }).locator(".sb-board-link");

  await firstBoard.click();
  const firstBucket = page.url().match(/#\/bucket\/(\d+)\/board/)?.[1];
  if (!firstBucket) throw new Error("first Board route has no bucket id");
  const firstSearch = page.locator(".board-route-view.is-active").getByRole("searchbox", { name: "search work items" });
  await firstSearch.fill("volume item 03");
  await expect.poll(() => page.url()).toContain("volume+item+03");

  await secondBoard.click();
  await expect(secondBoard).toHaveAttribute("aria-pressed", "true");
  await expect(firstBoard).toHaveAttribute("aria-pressed", "false");
  const secondSearch = page.locator(".board-route-view.is-active").getByRole("searchbox", { name: "search work items" });
  await expect(secondSearch).toHaveValue("");
  await secondSearch.fill("bucket-local-query");

  await firstBoard.click();
  await expect(page.locator(".board-route-view.is-active").getByRole("searchbox", { name: "search work items" })).toHaveValue("volume item 03");
  await page.evaluate((bucketId) => { location.hash = `/bucket/${bucketId}/item/1`; }, firstBucket);
  await expect(page.locator(".sidebar")).toBeVisible();
  await expect(page.getByRole("button", { name: /board, active/ })).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator(".board-route-view.is-active .workbench")).toBeVisible();
});

test("selected Board control has comfortable inset spacing and one clean edge", async ({ page }) => {
  await logIn(page);
  await page.setViewportSize({ width: 1440, height: 720 });

  const board = page.locator(".sb-board-link").first();
  await board.click();
  await expect(board).toHaveAttribute("aria-pressed", "true");

  const bucketRow = board.locator("xpath=ancestor::*[contains(@class, 'sb-bucket-row')]");
  await bucketRow.screenshot({ path: "test-results/item-112-selected-board.png" });

  const treatment = await board.evaluate((element) => {
    const style = getComputedStyle(element);
    return {
      paddingBlock: [parseFloat(style.paddingTop), parseFloat(style.paddingBottom)],
      paddingInline: [parseFloat(style.paddingLeft), parseFloat(style.paddingRight)],
      borderBottomWidth: parseFloat(style.borderBottomWidth),
      boxShadow: style.boxShadow,
    };
  });
  expect(treatment.paddingBlock).toEqual([2, 2]);
  expect(treatment.paddingInline).toEqual([6, 6]);
  expect(treatment.borderBottomWidth).toBe(1);
  expect(treatment.boxShadow).toBe("none");

  const workspace = page.locator(".workspace");
  for (const width of [304, 220]) {
    await workspace.evaluate((element, value) => {
      (element as HTMLElement).style.setProperty("--sidebar-w", `${value}px`);
    }, width);
    const containment = await board.evaluate((element) => {
      const button = element.getBoundingClientRect();
      const row = element.closest(".sb-bucket-row")!.getBoundingClientRect();
      return { buttonLeft: button.left, buttonRight: button.right, rowLeft: row.left, rowRight: row.right };
    });
    expect(containment.buttonLeft).toBeGreaterThanOrEqual(containment.rowLeft);
    expect(containment.buttonRight).toBeLessThanOrEqual(containment.rowRight);
  }
});

test("shared Sidebar footer wraps without overlap at minimum width and 200% zoom", async ({ page }) => {
  await logIn(page);
  await page.setViewportSize({ width: 1000, height: 720 });
  const workspace = page.locator(".workspace");
  const footer = page.locator(".sb-footer");

  for (const width of [304, 260, 220]) {
    await workspace.evaluate((element, value) => element.style.setProperty("--sidebar-w", `${value}px`), width);
    await expectFooterContained(footer);
  }

  const notification = footer.locator(".sb-notify-enable");
  if (await notification.count()) {
    await notification.evaluate((element) => { element.textContent = "enable notifications with a deliberately long localized label"; });
    await expectFooterContained(footer);
  }

  // A 720 CSS-pixel viewport is the layout seen by a 1440px-wide browser at 200% zoom.
  await page.setViewportSize({ width: 720, height: 500 });
  await expectFooterContained(footer);
  await expect(footer.getByLabel("show ended")).toBeEnabled();
  await footer.getByLabel("show ended").focus();
  await expect(footer.getByLabel("show ended")).toBeFocused();
});
