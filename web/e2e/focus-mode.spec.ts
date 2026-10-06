import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

const VIEWPORTS = [
  { width: 1440, height: 1000 },
  { width: 360, height: 800 },
] as const;

async function terminalBounds(page: Page) {
  return page.locator(".term-host").evaluate((element) => {
    const rect = element.getBoundingClientRect();
    return {
      top: rect.top,
      right: rect.right,
      bottom: rect.bottom,
      left: rect.left,
      width: rect.width,
      height: rect.height,
    };
  });
}

test("Focus mode gives the active session terminal the full content viewport", async ({ page, isolatedDaemon }) => {
  await page.addInitScript(() => localStorage.setItem("pm.terminal.debug", "1"));
  isolatedDaemon.seedSessionForward("browser-e2e", "preview");
  await isolatedDaemon.restart();
  await logIn(page);

  const session = page.locator(".sb-session").filter({
    has: page.locator(".sb-session-title").getByText("browser-e2e", { exact: true }),
  });
  await session.click();
  await expect(page.locator(".forwards-bar")).toBeVisible();
  await expect(page.locator(".terminal-debug-bar")).toBeVisible();
  await expect(page.locator(".terminal-debug-toggle")).toHaveCount(0);
  await expect(page.locator(".term-host")).toBeVisible();
  const sessionUrl = page.url();
  const terminalInput = page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
  await expect(terminalInput).toBeFocused();

  for (const [index, viewport] of VIEWPORTS.entries()) {
    await page.setViewportSize(viewport);
    const normal = await terminalBounds(page);
    expect(normal.top).toBeGreaterThan(0);
    expect(normal.height).toBeLessThan(viewport.height);

    await page.getByRole("button", { name: "enter focus mode" }).click();
    await expect(page.locator(".shell")).toHaveClass(/is-focus-mode/);
    await expect(page.getByRole("button", { name: "exit focus mode" })).toBeVisible();
    await expect(page.locator(".pane-head")).toBeHidden();
    await expect(page.locator(".terminal-tabs")).toBeHidden();
    await expect(page.locator(".forwards-bar")).toBeHidden();
    await expect(page.locator(".terminal-debug-bar")).toBeHidden();
    await expect(page).toHaveURL(`${sessionUrl}?focus=1`);
    await expect(terminalInput).toBeFocused();

    const focused = await terminalBounds(page);
    expect(focused).toEqual({
      top: 0,
      right: viewport.width,
      bottom: viewport.height,
      left: 0,
      width: viewport.width,
      height: viewport.height,
    });

    if (index === 0) await page.keyboard.press("Escape");
    else await page.getByRole("button", { name: "exit focus mode" }).click();
    await expect(page.locator(".shell")).not.toHaveClass(/is-focus-mode/);
    await expect(page.locator(".forwards-bar")).toBeVisible();
    await expect(page.locator(".terminal-debug-bar")).toBeVisible();
    await expect(page).toHaveURL(sessionUrl);
    await expect(terminalInput).toBeFocused();
  }
});
