import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

/// Counts the interaction reports the page sends. The heartbeat's
/// throttle and its own visibility check are unit-tested; what only a
/// real browser can settle is which events reach a capture-phase
/// document listener, and which never happen at all.
function activityReports(page: Page): () => number {
  let reports = 0;
  page.on("request", (request) => {
    if (request.method() !== "PUT") return;
    if (new URL(request.url()).pathname === "/api/user/activity") reports += 1;
  });
  return () => reports;
}

function visibleTerminalInput(page: Page) {
  return page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
}

test("an open, connected dashboard reports nothing until someone touches it", async ({ page }) => {
  const reports = activityReports(page);

  await logIn(page);
  const sessions = page.locator(".sb-session");
  await expect(sessions.first()).toBeVisible();

  expect(
    reports(),
    "a visible, focused, websocket-connected page is presence, not interaction",
  ).toBe(0);

  await sessions.first().click();

  await expect.poll(reports).toBe(1);
  await expect(visibleTerminalInput(page)).toBeAttached();
  expect(reports(), "a terminal painting its output is not interaction either").toBe(1);
});

test("typing into the embedded terminal counts as working here", async ({ page }) => {
  const reports = activityReports(page);

  await logIn(page);
  // Opened by dispatching the click rather than performing one, so the
  // burst below is the first real interaction the page has seen and the
  // throttle cannot hide it.
  await page.locator(".sb-session").first().evaluate((row: HTMLElement) => row.click());
  const input = visibleTerminalInput(page);
  await expect(input).toBeAttached();
  await input.focus();
  expect(reports()).toBe(0);

  // xterm consumes the keystroke at its own textarea, so this passes
  // only while the listeners stay in the capture phase.
  await page.keyboard.type("echo working");
  await expect.poll(reports).toBe(1);

  await page.keyboard.type(" some more");
  expect(reports(), "a typing burst is one report, not one per key").toBe(1);
});
