import { execFileSync } from "node:child_process";
import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

// The self-rendering agent erases the terminal scrollback on every repaint,
// which would corrupt the shared seeded sessions other specs depend on.
function spawnSession(title: string): void {
  execFileSync(process.env.PM_E2E_PM_BIN!, [
    "spawn", "--project", "1", "--agent", "codex", "--title", title,
  ], {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET! },
    encoding: "utf8",
  });
}

interface SocketMetric {
  bytes: number;
  created: number;
  closed: number;
  resizesSent: Array<{ cols: number; rows: number }>;
}

type ProbeWindow = Window & {
  __claudeProbe: { sockets: Record<string, SocketMetric> };
};

interface BufferReport {
  bufferLines: number;
  baseY: number;
  viewportY: number;
  firstHistoryLinePresent: boolean;
  lastHistoryIndexSeen: number;
  historyLinesInBuffer: number;
}

async function logInWithProbe(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const probe: ProbeWindow["__claudeProbe"] = { sockets: {} };
    (window as ProbeWindow).__claudeProbe = probe;
    const NativeWebSocket = window.WebSocket;
    window.WebSocket = new Proxy(NativeWebSocket, {
      construct(target, args) {
        const socket = Reflect.construct(target, args) as WebSocket;
        const url = String(args[0]);
        if (url.includes("/ws/terminal/")) {
          const metric = probe.sockets[url] ??= { bytes: 0, created: 0, closed: 0, resizesSent: [] };
          metric.created += 1;
          socket.addEventListener("close", () => metric.closed += 1);
          socket.addEventListener("message", (event) => {
            if (!(event.data instanceof ArrayBuffer) || event.data.byteLength < 10) return;
            if (new DataView(event.data).getUint8(0) !== 1) return;
            metric.bytes += event.data.byteLength - 10;
          });
          const nativeSend = socket.send.bind(socket);
          socket.send = ((payload: ArrayBuffer) => {
            if (payload instanceof ArrayBuffer && payload.byteLength === 13) {
              const view = new DataView(payload);
              if (view.getUint8(0) === 0x03) {
                metric.resizesSent.push({ cols: view.getUint16(9, true), rows: view.getUint16(11, true) });
              }
            }
            return nativeSend(payload);
          }) as typeof socket.send;
        }
        return socket;
      },
    }) as typeof WebSocket;
  });

  await logIn(page, { minimumSessions: 2 });
}

function sessionRow(page: Page, title: string) {
  return page.locator(".sb-session-title")
    .filter({ hasText: new RegExp(`^${title}$`) })
    .locator("xpath=ancestor::button[contains(@class, 'sb-session')][1]");
}

async function totalBytes(page: Page): Promise<number> {
  return page.evaluate(() => Object.values((window as ProbeWindow).__claudeProbe.sockets)
    .reduce((total, socket) => total + socket.bytes, 0));
}

async function waitForBytes(page: Page, previous: number, delta: number): Promise<void> {
  await page.waitForFunction(({ base, gain }) => Object.values(
    (window as ProbeWindow).__claudeProbe.sockets,
  ).reduce((total, socket) => total + socket.bytes, 0) >= base + gain, { base: previous, gain: delta }, { timeout: 60_000 });
}

async function typeCommand(page: Page, command: string): Promise<void> {
  const textarea = page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
  await textarea.focus();
  await page.keyboard.type(command);
  await page.keyboard.press("Enter");
}

/** Reads the visible layer's real xterm buffer through the stage. */
async function bufferReport(page: Page): Promise<BufferReport> {
  return page.evaluate(() => {
    interface StageShape {
      layers: Map<string, {
        el: HTMLElement;
        term: {
          buffer: {
            active: {
              length: number;
              baseY: number;
              viewportY: number;
              getLine(index: number): { translateToString(trim?: boolean): string } | undefined;
            };
          };
        };
      }>;
    }
    const stage = (window as unknown as { __pmStage?: StageShape }).__pmStage;
    if (!stage) throw new Error("stage probe missing");
    for (const layer of stage.layers.values()) {
      if (layer.el.style.visibility !== "visible" || !layer.el.isConnected) continue;
      const buffer = layer.term.buffer.active;
      let firstHistoryLinePresent = false;
      let lastHistoryIndexSeen = -1;
      let historyLinesInBuffer = 0;
      for (let index = 0; index < buffer.length; index += 1) {
        const text = buffer.getLine(index)?.translateToString(true) ?? "";
        const match = text.match(/CLAUDE-HISTORY-(\d{5})/);
        if (!match) continue;
        historyLinesInBuffer += 1;
        const historyIndex = Number.parseInt(match[1], 10);
        if (historyIndex === 0) firstHistoryLinePresent = true;
        if (historyIndex > lastHistoryIndexSeen) lastHistoryIndexSeen = historyIndex;
      }
      return {
        bufferLines: buffer.length,
        baseY: buffer.baseY,
        viewportY: buffer.viewportY,
        firstHistoryLinePresent,
        lastHistoryIndexSeen,
        historyLinesInBuffer,
      };
    }
    throw new Error("no visible layer");
  });
}

test("claude-style self-rendered transcript survives an away-and-return without a resize", async ({ page }) => {
  test.setTimeout(240_000);
  await logInWithProbe(page);
  spawnSession("agent-replay-a");
  spawnSession("agent-replay-b");
  const rowA = sessionRow(page, "agent-replay-a");
  const rowB = sessionRow(page, "agent-replay-b");
  await expect(rowA).toHaveCount(1);
  await expect(rowB).toHaveCount(1);

  await rowA.click();
  await expect(page.locator('.term-layer[style*="visible"]')).toBeVisible();
  // e2e-real-time-wait: allow the self-rendering agent's initial paint cadence to settle
  await page.waitForTimeout(300);

  // Full render of a 1500-line self-managed transcript, then 700 incremental
  // frames (~370 KiB) that only append and redraw the input box.
  const startBytes = await totalBytes(page);
  await typeCommand(page, "claudestream 1500 700 12 220");
  await waitForBytes(page, startBytes, 250_000);
  // e2e-real-time-wait: sample the deliberately paced stream while it is still active
  await page.waitForTimeout(300);

  const streaming = await bufferReport(page);
  expect(streaming.firstHistoryLinePresent, "transcript top must be in scrollback while watching").toBe(true);
  expect(streaming.baseY).toBeGreaterThan(700);

  await rowB.click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);

  // Let the stream finish so the agent is idle, like a Claude turn ending.
  // e2e-real-time-wait: claudestream intentionally models a 700-frame paced agent turn
  await page.waitForTimeout(6_000);

  // Return. The fresh layer replays the daemon ring tail, which contains
  // only incremental frames — no full render.
  await rowA.click();
  await expect(page.locator('.term-layer[style*="visible"]')).toBeVisible();
  // e2e-real-time-wait: allow the high-volume daemon replay to finish before inspecting xterm
  await page.waitForTimeout(1_000);

  const returned = await bufferReport(page);

  // Prove the data is recoverable: a browser resize sends SIGWINCH and the
  // agent full-renders, exactly the user's manual workaround.
  const viewport = page.viewportSize()!;
  await page.setViewportSize({ width: viewport.width + 80, height: viewport.height });
  // e2e-real-time-wait: allow the simulated agent's SIGWINCH repaint to complete
  await page.waitForTimeout(2_500);
  const resized = await bufferReport(page);

  const socketSummary = await page.evaluate(() => (window as ProbeWindow).__claudeProbe.sockets);
  console.log(`CLAUDE_REPLAY_METRICS ${JSON.stringify({ streaming, returned, resized, socketSummary })}`);

  expect(resized.firstHistoryLinePresent, "resize must restore the transcript").toBe(true);
  expect(resized.baseY).toBeGreaterThan(700);

  // The contract under test: returning must show the transcript without a
  // resize. On the broken build the replay tail cannot reconstruct it.
  expect(returned.firstHistoryLinePresent, "transcript top must survive away-and-return").toBe(true);
  expect(returned.baseY).toBeGreaterThan(700);
});
