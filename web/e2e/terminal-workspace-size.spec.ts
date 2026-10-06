import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

type LayerSnapshot = {
  key: string;
  visible: boolean;
  cols: number;
  rows: number;
  lastPtyResize: { cols: number; rows: number } | null;
  socket: {
    ptySize: { cols: number; rows: number } | null;
    resizesSent: Array<{ cols: number; rows: number; at: number }>;
  } | null;
};

type StageWindow = Window & {
  __pmStage?: { debugSnapshot(): LayerSnapshot[] };
};

const AGENT_PANE_LABEL = "browser-e2e · agent";

async function openSessionView(page: Page): Promise<void> {
  await page.locator(".sb-session-title")
    .filter({ hasText: /^browser-e2e$/ })
    .locator("xpath=ancestor::button[contains(@class, 'sb-session')][1]")
    .click();
  await expect(page.locator('.term-layer[style*="visible"]')).toBeVisible();
}

/** The visible layer once the daemon's PTY size echo matches its fit,
 * i.e. the resize round trip has settled. */
async function settledVisibleLayer(page: Page): Promise<LayerSnapshot> {
  await expect.poll(() => page.evaluate(() => {
    const entry = (window as StageWindow).__pmStage?.debugSnapshot().find((layer) => layer.visible);
    const size = entry?.socket?.ptySize;
    return Boolean(size && size.cols === entry!.cols && size.rows === entry!.rows);
  })).toBe(true);
  return await page.evaluate(() =>
    (window as StageWindow).__pmStage!.debugSnapshot().find((layer) => layer.visible)!);
}

test("returning from a workspace pane restores the session-view PTY size", async ({ page }) => {
  await logIn(page);
  await openSessionView(page);
  const session = await settledVisibleLayer(page);
  const sentBefore = session.socket!.resizesSent.length;

  // View the same agent PTY from a half-width workspace pane, which
  // resizes it away from the session-view geometry.
  await page.getByTitle("new workspace").click();
  await page.getByRole("button", { name: "create workspace" }).click();
  await expect(page.locator(".workspace-pane")).toHaveCount(1);
  await page.getByTitle("split right").first().click();
  await expect(page.locator(".workspace-pane")).toHaveCount(2);
  await page.getByLabel("terminal shown in pane").first().selectOption({ label: AGENT_PANE_LABEL });

  // The warm session layer hears the pane's resize through the daemon's
  // size echo: its PTY mirror leaves the session-view geometry without
  // any resize being sent on the session layer's own socket.
  await expect.poll(() => page.evaluate((expected) => {
    const entry = (window as StageWindow).__pmStage?.debugSnapshot()
      .find((layer) => layer.key === expected.key);
    const size = entry?.lastPtyResize;
    return Boolean(size && (size.cols !== expected.cols || size.rows !== expected.rows));
  }, { key: session.key, cols: session.cols, rows: session.rows })).toBe(true);
  const paneSize = await page.evaluate((key) =>
    (window as StageWindow).__pmStage!.debugSnapshot()
      .find((layer) => layer.key === key)!.lastPtyResize!, session.key);

  // Back in the session view, the stage must notice the fit no longer
  // matches the PTY and send one corrective resize, ending with the PTY
  // at the session-view geometry again.
  await openSessionView(page);
  await expect.poll(() => page.evaluate((expected) => {
    const entry = (window as StageWindow).__pmStage?.debugSnapshot()
      .find((layer) => layer.visible);
    if (!entry || entry.key !== expected.key) return "wrong layer";
    if (entry.cols !== expected.cols || entry.rows !== expected.rows) return "fit changed";
    const size = entry.socket?.ptySize;
    if (!size || size.cols !== expected.cols || size.rows !== expected.rows) {
      return "pty not corrected";
    }
    const last = entry.socket!.resizesSent.at(-1);
    if (!last || last.cols !== expected.cols || last.rows !== expected.rows) {
      return "no corrective resize sent";
    }
    return "corrected";
  }, { key: session.key, cols: session.cols, rows: session.rows })).toBe("corrected");

  const after = await settledVisibleLayer(page);
  expect(after.key).toBe(session.key);
  expect({ cols: after.cols, rows: after.rows }).toEqual({ cols: session.cols, rows: session.rows });
  expect({ cols: paneSize.cols, rows: paneSize.rows })
    .not.toEqual({ cols: session.cols, rows: session.rows });
  // The correction was actively sent by the returning viewer, not a no-op.
  expect(after.socket!.resizesSent.length).toBeGreaterThan(sentBefore);
});
