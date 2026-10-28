import { expect, test, type Page } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";

interface ViewportState {
  baseY: number;
  viewportY: number;
  backgroundBytes: number;
  marker: string;
}

interface StageWindow {
  __pmStage?: {
    debugSnapshot(): Array<{ key: string; visible: boolean; baseY: number; viewportY: number; backgroundBytes: number }>;
    layers: Map<string, {
      term: {
        renderSnapshot(): { rowData: Array<{ text: string }> } | null;
        scrollToLine(line: number): void;
      };
    }>;
  };
}

type MetricWindow = Window & {
  __terminalScrollBytes: number;
  __terminalScrollTail: string;
  __terminalScrollIndex: number;
};

const TERMINAL_READY_TIMEOUT_MS = 20_000;

async function state(page: Page): Promise<ViewportState | null> {
  return page.evaluate(() => {
    const stage = (window as unknown as StageWindow).__pmStage;
    const debug = stage?.debugSnapshot().find((candidate) => candidate.visible);
    const layer = debug ? stage?.layers.get(debug.key) : undefined;
    if (!debug || !layer) return null;
    let marker = "";
    for (const row of layer.term.renderSnapshot()?.rowData.slice(0, 8) ?? []) {
      const text = row.text;
      if (text.includes("CODEX-SYNC-")) {
        marker = text.match(/CODEX-SYNC-\d+/)?.[0] ?? "";
        break;
      }
    }
    return { ...debug, marker };
  });
}

async function readyState(page: Page): Promise<ViewportState> {
  let viewport = await state(page);
  await expect.poll(async () => {
    viewport = await state(page);
    return viewport;
  }, {
    message: "waiting for the visible terminal viewport to become ready",
    timeout: TERMINAL_READY_TIMEOUT_MS,
  }).not.toBeNull();
  if (!viewport) throw new Error("visible terminal viewport did not become ready");
  return viewport;
}

async function typeCommand(page: Page, command: string): Promise<void> {
  const textarea = page.locator('.term-layer[style*="visible"] .ghostty-input');
  await textarea.focus();
  await page.keyboard.type(command);
  await page.keyboard.press("Enter");
}

/** Deliberately does not wait for the first output: the caller needs this
 * stream still arriving after it switches away, and waiting here lets a short
 * burst finish while the layer is still visible. */
async function startSyncOutput(page: Page, command: string): Promise<number> {
  const bytes = await page.evaluate(() => (window as unknown as MetricWindow).__terminalScrollBytes);
  await readyState(page);
  await typeCommand(page, command);
  return bytes;
}

async function expectAtBottom(page: Page): Promise<void> {
  await readyState(page);
  await expect.poll(async () => {
    const viewport = await state(page);
    return viewport ? viewport.baseY - viewport.viewportY : null;
  }).toBe(0);
}

async function switchToOtherSessionAndBack(page: Page): Promise<void> {
  const sessions = page.locator(".sb-session");
  await sessions.nth(1).click();
  await expect(sessions.nth(1)).toHaveClass(/is-selected/);
  await sessions.nth(0).click();
  await expect(sessions.nth(0)).toHaveClass(/is-selected/);
}

async function waitForHiddenLayer(page: Page, key: string): Promise<void> {
  await expect.poll(async () => {
    const snapshots = await page.evaluate(() => (window as unknown as StageWindow).__pmStage?.debugSnapshot() ?? []);
    return snapshots.find((candidate) => candidate.key === key)?.visible;
  }).toBe(false);
}

async function boardDetourAndReturn(page: Page, sessionId: string): Promise<void> {
  await page.locator(".sb-board-link").click();
  await expect(page).toHaveURL(/#\/bucket\/1\/board/);
  await page.locator(".sb-session").nth(0).click();
  await expect(page).toHaveURL(new RegExp(`#\\/session\\/${sessionId}$`));
}

async function installTerminalStreamProbe(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const probe = window as unknown as MetricWindow;
    probe.__terminalScrollBytes = 0;
    probe.__terminalScrollTail = "";
    probe.__terminalScrollIndex = -1;
    const NativeWebSocket = window.WebSocket;
    window.WebSocket = new Proxy(NativeWebSocket, {
      construct(target, args) {
        const socket = Reflect.construct(target, args) as WebSocket;
        if (!String(args[0]).includes("/ws/terminal/")) return socket;
        socket.addEventListener("message", (event) => {
          if (!(event.data instanceof ArrayBuffer) || event.data.byteLength < 10) return;
          if (new DataView(event.data).getUint8(0) !== 1) return;
          const bytes = new Uint8Array(event.data, 10);
          probe.__terminalScrollBytes += bytes.byteLength;
          const text = probe.__terminalScrollTail + new TextDecoder().decode(bytes);
          for (const match of text.matchAll(/CODEX-SYNC-(\d+)/g)) {
            probe.__terminalScrollIndex = Math.max(probe.__terminalScrollIndex, Number(match[1]));
          }
          probe.__terminalScrollTail = text.slice(-32);
        });
        return socket;
      },
    }) as typeof WebSocket;
  });
}

test("codex-style output preserves bottom following and scrolled markers across returns", async ({ page }) => {
  await installTerminalStreamProbe(page);
  await logIn(page, { minimumSessions: 2 });
  const sessions = page.locator(".sb-session");
  await sessions.nth(0).click();
  await expect(page.locator('.term-layer[style*="visible"]')).toBeVisible();
  const sessionId = page.url().match(/#\/session\/(\d+)$/)?.[1];
  if (!sessionId) throw new Error("seeded session did not open");

  const belowCapStart = await startSyncOutput(page, "syncout 900 60");
  await sessions.nth(1).click();
  await waitForHiddenLayer(page, `s:${sessionId}`);
  await expect.poll(() => page.evaluate(() => (window as unknown as MetricWindow).__terminalScrollBytes))
    .toBeGreaterThan(belowCapStart + 15_000);
  await expect.poll(async () => {
    const snapshots = await page.evaluate(() => (window as unknown as StageWindow).__pmStage?.debugSnapshot() ?? []);
    return snapshots.find((candidate) => candidate.key === `s:${sessionId}`)?.baseY ?? 0;
  }).toBeGreaterThan(0);
  await sessions.nth(0).click();
  await expectAtBottom(page);

  // Keep output arriving while the active viewport is hidden.
  const aboveCapStart = await startSyncOutput(page, "syncout 6200 5");
  await sessions.nth(1).click();
  await waitForHiddenLayer(page, `s:${sessionId}`);
  await expect.poll(() => page.evaluate(() => (window as unknown as MetricWindow).__terminalScrollBytes))
    .toBeGreaterThan(aboveCapStart + 120_000);
  await sessions.nth(0).click();
  await expect.poll(() => state(page).then((viewport) => viewport?.baseY)).toBeGreaterThan(5_000);
  await expectAtBottom(page);

  // The Board route keeps the layer live but CSS-hidden while more sync frames parse.
  await typeCommand(page, "syncout 300 2");
  await page.locator(".sb-board-link").click();
  await expect(page).toHaveURL(/#\/bucket\/1\/board/);
  await expect.poll(async () => {
    const snapshots = await page.evaluate(() => (window as unknown as StageWindow).__pmStage?.debugSnapshot() ?? []);
    return snapshots.find((candidate) => candidate.key === `s:${sessionId}`)?.baseY ?? 0;
  }).toBeGreaterThan(5_000);
  await page.locator(".sb-session").nth(0).click();
  await expectAtBottom(page);

  // A fresh terminal receives the server snapshot; the zero-distance bookmark
  // must explicitly clear any spurious user-scroll state after parsing it.
  await page.reload();
  await expect(page).toHaveURL(new RegExp(`#\\/session\\/${sessionId}$`));
  await readyState(page);
  await expect.poll(() => state(page).then((viewport) => viewport?.baseY)).toBeGreaterThanOrEqual(4_900);
  await expectAtBottom(page);

  // Distance-from-bottom is shared by warm shows and reload replay. Verify the
  // content marker, not merely a scrollbar coordinate, across each return flow.
  await page.evaluate(() => {
    const stage = (window as unknown as StageWindow).__pmStage;
    const debug = stage?.debugSnapshot().find((candidate) => candidate.visible);
    if (!debug) throw new Error("missing terminal debug state");
    stage?.layers.get(debug.key)?.term.scrollToLine(debug.baseY - 120);
  });
  await expect.poll(() => state(page).then((viewport) => viewport?.marker)).toMatch(/CODEX-SYNC-\d+/);
  const marker = (await readyState(page)).marker;
  await switchToOtherSessionAndBack(page);
  await expect.poll(() => state(page).then((viewport) => viewport?.marker)).toBe(marker);
  await boardDetourAndReturn(page, sessionId);
  await expect.poll(() => state(page).then((viewport) => viewport?.marker)).toBe(marker);
  await page.reload();
  await readyState(page);
  await expect.poll(() => state(page).then((viewport) => viewport?.marker)).toBe(marker);
});

/** Lines that must arrive while the reader is scrolled up. */
const LINES_WHILE_READING = 300;

async function topRowText(page: Page): Promise<string> {
  return page.evaluate(() => {
    const stage = (window as unknown as StageWindow).__pmStage;
    const debug = stage?.debugSnapshot().find((candidate) => candidate.visible);
    return debug ? stage?.layers.get(debug.key)?.term.renderSnapshot()?.rowData[0]?.text ?? "" : "";
  });
}

/** The highest CODEX-SYNC index printed so far, from the buffer tail. */
async function lastSyncIndex(page: Page): Promise<number> {
  return page.evaluate(() => (window as unknown as MetricWindow).__terminalScrollIndex);
}

test("live output leaves the text a scrolled-up reader is on in place", async ({ page }) => {
  await installTerminalStreamProbe(page);
  await logIn(page, { minimumSessions: 1 });
  await page.locator(".sb-session").nth(0).click();
  await expectTerminalRevealed(page, "s:");

  // A long live stream must keep the terminal's viewport position coherent.
  const preloadLines = 5200;
  await typeCommand(page, `syncout ${preloadLines} 0`);
  await expect.poll(() => state(page).then((viewport) => viewport?.baseY), {
    timeout: TERMINAL_READY_TIMEOUT_MS,
  }).toBeGreaterThan(5_000);
  await expect.poll(() => lastSyncIndex(page), { timeout: TERMINAL_READY_TIMEOUT_MS }).toBe(preloadLines - 1);
  await expectAtBottom(page);

  await page.evaluate(() => {
    const probe = window as unknown as MetricWindow;
    probe.__terminalScrollIndex = -1;
    probe.__terminalScrollTail = "";
  });
  await typeCommand(page, "syncout 2000 100");
  await expect.poll(() => lastSyncIndex(page), { timeout: TERMINAL_READY_TIMEOUT_MS }).toBeGreaterThanOrEqual(0);
  await expect.poll(() => lastSyncIndex(page), { timeout: TERMINAL_READY_TIMEOUT_MS })
    .toBeLessThan(LINES_WHILE_READING);

  const layer = page.locator('.term-layer[style*="visibility: visible"]');
  const box = await layer.boundingBox();
  if (!box) throw new Error("visible terminal has no box");
  await page.mouse.move(box.x + box.width / 2, box.y + box.height / 2);
  await page.mouse.wheel(0, -2_000);
  await expect.poll(async () => {
    const viewport = await state(page);
    return viewport ? viewport.baseY - viewport.viewportY : 0;
  }).toBeGreaterThan(20);
  const reading = await topRowText(page);
  expect(reading).toMatch(/CODEX-SYNC-\d+/);

  const arrived = await lastSyncIndex(page);
  await expect.poll(() => lastSyncIndex(page), { timeout: TERMINAL_READY_TIMEOUT_MS })
    .toBeGreaterThan(arrived + LINES_WHILE_READING);
  expect(await topRowText(page)).toBe(reading);
});
