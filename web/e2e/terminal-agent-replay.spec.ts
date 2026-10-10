import { execFileSync } from "node:child_process";
import { expect, test, type Page } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";

const STREAM_DONE_MARKER = "CLAUDE-STREAM-DONE";
const STREAM_DONE_TIMEOUT_MS = 60_000;
const MIN_TRANSCRIPT_BASE_Y = 700;
const RESIZE_WIDTH_STEP_PX = 80;
const TRANSCRIPT_RESTORED = { firstHistoryLinePresent: true, deepScrollback: true };

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

function transcriptState(report: BufferReport): typeof TRANSCRIPT_RESTORED {
  return {
    firstHistoryLinePresent: report.firstHistoryLinePresent,
    deepScrollback: report.baseY > MIN_TRANSCRIPT_BASE_Y,
  };
}

async function layerContains(page: Page, key: string, text: string): Promise<boolean> {
  return page.evaluate(({ layerKey, needle }) => {
    const stage = (window as unknown as { __pmStage?: { layers: Map<string, { term: { buffer: { active: {
      length: number;
      getLine(index: number): { translateToString(trim?: boolean): string } | undefined;
    } } } }> } }).__pmStage;
    const buffer = stage?.layers.get(layerKey)?.term.buffer.active;
    if (!buffer) return false;
    for (let index = 0; index < buffer.length; index += 1) {
      if (buffer.getLine(index)?.translateToString(true).includes(needle)) return true;
    }
    return false;
  }, { layerKey: key, needle: text });
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
  const keyA = await expectTerminalRevealed(page, "s:");
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
  expect(streaming.baseY).toBeGreaterThan(MIN_TRANSCRIPT_BASE_Y);

  await rowB.click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);

  // The away layer stays attached, so it shows when the agent turn has ended.
  await expect.poll(() => layerContains(page, keyA, STREAM_DONE_MARKER), {
    message: "waiting for the paced agent turn to finish",
    timeout: STREAM_DONE_TIMEOUT_MS,
  }).toBe(true);

  // Return. The layer replays the daemon ring tail, which contains only
  // incremental frames — no full render.
  await rowA.click();
  await expectTerminalRevealed(page, keyA);

  // The contract under test: returning must show the transcript without a
  // resize. On the broken build the replay tail cannot reconstruct it.
  let returned = await bufferReport(page);
  await expect.poll(async () => {
    returned = await bufferReport(page);
    return transcriptState(returned);
  }, { message: "transcript top must survive away-and-return" }).toEqual(TRANSCRIPT_RESTORED);

  // Prove the data is recoverable: a browser resize sends SIGWINCH and the
  // agent full-renders, exactly the user's manual workaround.
  const viewport = page.viewportSize()!;
  await page.setViewportSize({ width: viewport.width + RESIZE_WIDTH_STEP_PX, height: viewport.height });
  let resized = await bufferReport(page);
  await expect.poll(async () => {
    resized = await bufferReport(page);
    return transcriptState(resized);
  }, { message: "resize must restore the transcript" }).toEqual(TRANSCRIPT_RESTORED);

  const socketSummary = await page.evaluate(() => (window as ProbeWindow).__claudeProbe.sockets);
  console.log(`CLAUDE_REPLAY_METRICS ${JSON.stringify({ streaming, returned, resized, socketSummary })}`);
});
