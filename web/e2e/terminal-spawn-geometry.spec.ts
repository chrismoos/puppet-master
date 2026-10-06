import { expect, test } from "./fixtures";
import { logIn } from "./support";

/** The daemon's own default when a spawn carries no geometry. */
const DAEMON_DEFAULT_COLS = 120;
const VIEWER_WIDTH = 1500;
const VIEWER_HEIGHT = 900;

interface StageWindow extends Window {
  __pmStage?: {
    currentSize(): { cols: number; rows: number } | null;
    debugSnapshot(): Array<{ key: string; visible: boolean }>;
    disposeSession(sessionId: bigint): void;
    layers: Map<string, { term: { cols: number; rows: number } }>;
  };
}

function readStage(page: import("@playwright/test").Page) {
  return page.evaluate(() => {
    const stage = (window as StageWindow).__pmStage;
    const visible = stage?.debugSnapshot().find((entry) => entry.visible);
    const pane = visible ? stage?.layers.get(visible.key)?.term : undefined;
    return {
      geometry: stage?.currentSize() ?? null,
      pane: pane ? { cols: pane.cols, rows: pane.rows } : null,
      panes: stage?.debugSnapshot().length ?? 0,
      terminals: document.querySelectorAll(".xterm").length,
    };
  });
}

test.use({ viewport: { width: VIEWER_WIDTH, height: VIEWER_HEIGHT } });

test("spawn geometry comes from the mounted pane and the stage holds no other terminal", async ({ page }) => {
  await logIn(page);
  await page.locator(".sb-session").first().click();
  const visible = page.locator('.term-layer[style*="visibility: visible"] .xterm');
  await expect(visible).toBeVisible();

  const mounted = await readStage(page);
  expect(mounted.panes, "one session pane is mounted").toBe(1);
  // The regression this guards: a measurement terminal parked in the stage
  // answers to .xterm alongside the pane and breaks every pane selector.
  expect(mounted.terminals, "the stage holds exactly the mounted panes").toBe(mounted.panes);
  expect(mounted.pane, "the visible pane reports its own size").not.toBeNull();
  expect(mounted.geometry, "the mounted pane is the geometry a spawn asks for")
    .toEqual(mounted.pane);
  expect(mounted.pane!.cols, "a measured pane, not the daemon default")
    .toBeGreaterThan(DAEMON_DEFAULT_COLS);
});

test("measuring with no pane mounted leaves no terminal behind in the stage", async ({ page }) => {
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await expect(page.locator('.term-layer[style*="visibility: visible"] .xterm')).toBeVisible();

  const sessionId = page.url().match(/#\/session\/(\d+)/)?.[1];
  if (!sessionId) throw new Error(`session did not open: ${page.url()}`);

  // Emptying the stage while its host stays mounted is the one state that
  // reaches the measurement probe, which is otherwise never the source.
  const probed = await page.evaluate((id) => {
    const stage = (window as StageWindow).__pmStage;
    stage?.disposeSession(BigInt(id));
    const before = document.querySelectorAll(".xterm").length;
    const geometry = stage?.currentSize() ?? null;
    return { before, geometry, after: document.querySelectorAll(".xterm").length };
  }, sessionId);

  expect(probed.before, "the stage is empty before measuring").toBe(0);
  expect(probed.geometry, "the probe measured the stage").not.toBeNull();
  expect(probed.geometry!.cols, "a measured probe, not the daemon default")
    .toBeGreaterThan(DAEMON_DEFAULT_COLS);
  expect(probed.after, "the probe is gone once the measurement returns").toBe(0);
  await expect(page.locator(".xterm")).toHaveCount(0);
});
