import { expect, test } from "./fixtures";
import { logIn } from "./support";

function sessionRow(page: import("@playwright/test").Page, title: string) {
  return page.locator(".sb-session-title", { hasText: new RegExp(`^${title}$`) })
    .locator("xpath=ancestor::button[contains(@class, 'sb-session')][1]");
}

test("ended and failed grace expires without evicting the selected session", async ({ page, isolatedDaemon }) => {
  await page.addInitScript(() => {
    if (localStorage.getItem("pm.showEnded") === null) localStorage.setItem("pm.showEnded", "false");
  });
  await logIn(page);

  const failed = sessionRow(page, "browser-e2e");
  await failed.click();
  const textarea = page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
  await textarea.focus();
  await page.keyboard.type("exit 7");
  await page.keyboard.press("Enter");
  await expect(failed).toContainText("ended ·");

  isolatedDaemon.setSessionEndedState("browser-e2e", "exited", 61_000);
  await page.reload();
  await expect(failed).toBeVisible();
  await expect(failed).toHaveClass(/is-selected/);

  await sessionRow(page, "browser-e2e-two").click();
  await expect(failed).toHaveCount(0);

  isolatedDaemon.setSessionEndedState("browser-e2e-two", "failed");
  await page.reload();
  await expect(sessionRow(page, "browser-e2e-two")).toContainText("failed ·");
});

test("bounded history pages, grouped metadata search, and deep retention", async ({ page, isolatedDaemon }) => {
  isolatedDaemon.seedEndedSessions(120, 128 * 1024);
  await isolatedDaemon.restart();
  await page.addInitScript(() => {
    if (localStorage.getItem("pm.showEnded") === null) localStorage.setItem("pm.showEnded", "false");
    (window as Window & { __controlFrames: number[] }).__controlFrames = [];
    const NativeWebSocket = window.WebSocket;
    window.WebSocket = new Proxy(NativeWebSocket, {
      construct(target, args) {
        const socket = Reflect.construct(target, args) as WebSocket;
        if (String(args[0]).endsWith("/ws")) socket.addEventListener("message", (event) => {
          if (event.data instanceof ArrayBuffer) {
            (window as Window & { __controlFrames: number[] }).__controlFrames.push(event.data.byteLength);
          }
        });
        return socket;
      },
    }) as typeof WebSocket;
  });
  await logIn(page);

  const rows = page.locator(".sb-session");
  await expect(rows).toHaveCount(2);
  expect(await page.evaluate(() => Math.max(...(window as Window & { __controlFrames: number[] }).__controlFrames))).toBeLessThan(1_000_000);
  await page.getByLabel("show ended").check();
  await expect(rows).toHaveCount(52);
  expect(await page.evaluate(() => Math.max(...(window as Window & { __controlFrames: number[] }).__controlFrames))).toBeLessThan(7_000_000);
  // Showing ended sessions pages them into the list without a count beside
  // the search box: the count there is for search results only.
  await expect(page.locator(".sb-search [role=status]")).toHaveText("");

  await page.getByRole("searchbox", { name: "Search sessions" }).fill("old searchable needle");
  await expect(rows).toHaveCount(1);
  await expect(page.locator(".sb-bucket-head", { hasText: "browser-e2e" })).toBeVisible();
  await expect(page.locator(".sb-session-title").first()).toBeVisible();
  const oldRow = rows.first();
  await expect(oldRow).toContainText("ended ·");
  await oldRow.click();
  const oldRoute = page.url();

  await page.reload();
  await expect(page).toHaveURL(oldRoute);
  await expect(page.locator(".sb-session.is-selected")).toContainText("old searchable needle");

  await page.getByRole("searchbox", { name: "Search sessions" }).fill("");
  await expect(rows).toHaveCount(53);
  await sessionRow(page, "browser-e2e").click();
  await expect(rows).toHaveCount(52);
  await page.getByRole("button", { name: "load more" }).click();
  await expect(rows).toHaveCount(102);
});
