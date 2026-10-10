import { execFileSync } from "node:child_process";
import { expect, test, type Page } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";

function cli(args: string[]): string {
  return execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
}

async function openActiveBoardControl(page: Page) {
  return page.getByRole("button", { name: /board, active/ });
}

test("active Board control toggles back with terminal state intact", async ({ page }) => {
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await expect(page.locator(".term-host .xterm-screen")).toBeVisible();

  await page.getByRole("button", { name: "+ Shell" }).click();
  const shellTab = page.locator(".terminal-tab").last();
  await expect(shellTab).toHaveClass(/active/);
  await expectTerminalRevealed(page, "t:");
  const visibleTerminal = page.locator('.term-layer[style*="visibility: visible"]');
  await visibleTerminal.locator(".xterm-helper-textarea").pressSequentially("seq 1 300");
  await page.keyboard.press("Enter");
  await expect.poll(() => page.evaluate(() => {
    const stage = (window as Window & { __pmStage?: { debugSnapshot(): Array<{ visible: boolean; baseY: number }> } }).__pmStage;
    return stage?.debugSnapshot().find((entry) => entry.visible)?.baseY ?? 0;
  })).toBeGreaterThan(200);
  await visibleTerminal.locator("canvas.xterm-link-layer").hover();
  await page.mouse.wheel(0, -1200);
  await visibleTerminal.evaluate((node) => { node.setAttribute("data-board-viewport-probe", "retained"); });

  const board = page.getByRole("button", { name: "open board" });
  await board.focus();
  await page.keyboard.press("Enter");
  await expect(page.locator(".workbench")).toBeVisible();
  await expect(page.locator(".restorable-view.is-board-background")).toHaveCount(1);
  const activeBoard = await openActiveBoardControl(page);
  await expect(activeBoard).toHaveAttribute("aria-pressed", "true");

  const search = page.getByRole("searchbox", { name: "search work items" });
  await search.fill("volume item 01");
  await expect.poll(() => page.url()).toContain("q=volume+item+01");
  await activeBoard.focus();
  await page.keyboard.press("Space");
  await expect(shellTab).toHaveClass(/active/);
  await expect(page.locator('[data-board-viewport-probe="retained"]')).toBeVisible();

  await page.getByRole("button", { name: "open board" }).click();
  const bucketId = page.url().match(/#\/bucket\/(\d+)\/board/)?.[1];
  if (!bucketId) throw new Error(`Board route has no bucket: ${page.url()}`);
  await page.evaluate((bucket) => { location.hash = `/bucket/${bucket}/item/1`; }, bucketId);
  await expect(page).toHaveURL(new RegExp(`#/bucket/${bucketId}/item/1$`));
  await (await openActiveBoardControl(page)).click();
  await expect(shellTab).toHaveClass(/active/);
  // The tab rides in the address, which is what makes the shell above
  // still be the selected one after the round trip.
  await expect(page).toHaveURL(/#\/session\/\d+(\?tab=\d+)?$/);
  await page.getByRole("button", { name: "close", exact: true }).click();
  await expect(page.locator(".terminal-tab")).toHaveCount(0);
});

test("Board without valid memory falls back to application home", async ({ page }) => {
  await logIn(page);
  await page.goto(`${process.env.PM_E2E_BASE_URL!}/#/bucket/1/board`);
  await expect(page.locator(".workbench")).toBeVisible();
  await (await openActiveBoardControl(page)).click();
  await expect(page).toHaveURL(/#\/$/);
  await expect(page.getByText("select a session", { exact: true })).toBeVisible();
});

test("Board top-nav Home restores the previously open session and rejects a stale one", async ({ page }) => {
  await logIn(page);
  await page.locator(".sb-session").first().click();
  const sessionUrl = page.url();
  const sessionId = sessionUrl.match(/#\/session\/(\d+)$/)?.[1];
  if (!sessionId) throw new Error(`session did not open: ${sessionUrl}`);

  await page.getByRole("link", { name: "Home" }).click();
  await expect(page.getByText("select a session", { exact: true })).toBeVisible();
  await page.getByRole("button", { name: "open board" }).click();
  await expect(page.locator(".workbench")).toBeVisible();
  await page.getByRole("link", { name: "Home" }).click();
  await expect(page).toHaveURL(sessionUrl);

  await page.getByRole("button", { name: "open board" }).click();
  cli(["kill", sessionId]);
  await expect(page.locator(".sb-session.is-selected .state-dot")).toHaveClass(/st-(exited|failed)/);
  await page.getByRole("link", { name: "Home" }).click();
  await expect(page).toHaveURL(/#\/$/);
  await expect(page.getByText("select a session", { exact: true })).toBeVisible();
});
