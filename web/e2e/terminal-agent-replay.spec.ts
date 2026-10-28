import { execFileSync } from "node:child_process";
import { TERMINAL_TAG_OUTPUT } from "@puppet-master/client-core/ws/terminalFrame";
import { expect, test, type Page } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";

const STREAM_DONE_MARKER = "CLAUDE-STREAM-DONE";
const STREAM_DONE_TIMEOUT_MS = 60_000;
const HISTORY_PAINT_TIMEOUT_MS = 10_000;
const STREAM_PROBE_TAIL_CHARACTERS = 4_096;
const TERMINAL_OUTPUT_PAYLOAD_OFFSET = 10;
const HISTORY_SCAN_SAMPLE_LIMIT = 16;
const HISTORY_SCAN_ROW_CHARACTERS = 180;
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
  textTail: string;
  created: number;
  closed: number;
  resizesSent: Array<{ cols: number; rows: number }>;
  historyTopReceived: boolean;
  events: Array<{ at: number; kind: string; bytes?: number; flags?: number }>;
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
  diagnostics: unknown;
}

async function logInWithProbe(page: Page): Promise<void> {
  await page.addInitScript(({ tailCharacters, outputTag, payloadOffset, eventLimit }) => {
    const probe: ProbeWindow["__claudeProbe"] = { sockets: {} };
    (window as unknown as ProbeWindow).__claudeProbe = probe;
    const NativeWebSocket = window.WebSocket;
    window.WebSocket = new Proxy(NativeWebSocket, {
      construct(target, args) {
        const socket = Reflect.construct(target, args) as WebSocket;
        const url = String(args[0]);
        if (url.includes("/ws/terminal/")) {
          const metric = probe.sockets[url] ??= {
            bytes: 0, textTail: "", created: 0, closed: 0, resizesSent: [], historyTopReceived: false, events: [],
          };
          const decoder = new TextDecoder();
          const record = (event: SocketMetric["events"][number]) => {
            metric.events.push(event);
            if (metric.events.length > eventLimit) metric.events.splice(Math.floor(eventLimit / 2), 1);
          };
          metric.created += 1;
          socket.addEventListener("open", () => record({ at: performance.now(), kind: "open" }));
          socket.addEventListener("close", () => {
            metric.closed += 1;
            record({ at: performance.now(), kind: "close" });
          });
          socket.addEventListener("message", (event) => {
            if (!(event.data instanceof ArrayBuffer) || event.data.byteLength < payloadOffset) return;
            if (new DataView(event.data).getUint8(0) !== outputTag) return;
            metric.bytes += event.data.byteLength - payloadOffset;
            const text = metric.textTail + decoder.decode(new Uint8Array(event.data, payloadOffset), { stream: true });
            metric.historyTopReceived ||= text.includes("CLAUDE-HISTORY-00000");
            metric.textTail = text.slice(-tailCharacters);
            record({
              at: performance.now(), kind: "output", bytes: event.data.byteLength - payloadOffset,
              flags: new DataView(event.data).getUint8(payloadOffset - 1),
            });
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
  }, {
    tailCharacters: STREAM_PROBE_TAIL_CHARACTERS,
    outputTag: TERMINAL_TAG_OUTPUT,
    payloadOffset: TERMINAL_OUTPUT_PAYLOAD_OFFSET,
    eventLimit: HISTORY_SCAN_SAMPLE_LIMIT,
  });

  await logIn(page, { minimumSessions: 2 });
}

function sessionRow(page: Page, title: string) {
  return page.locator(".sb-session-title")
    .filter({ hasText: new RegExp(`^${title}$`) })
    .locator("xpath=ancestor::button[contains(@class, 'sb-session')][1]");
}

async function totalBytes(page: Page): Promise<number> {
  return page.evaluate(() => Object.values((window as unknown as ProbeWindow).__claudeProbe.sockets)
    .reduce((total, socket) => total + socket.bytes, 0));
}

async function waitForBytes(page: Page, previous: number, delta: number): Promise<void> {
  await page.waitForFunction(({ base, gain }) => Object.values(
    (window as unknown as ProbeWindow).__claudeProbe.sockets,
  ).reduce((total, socket) => total + socket.bytes, 0) >= base + gain, { base: previous, gain: delta }, { timeout: 60_000 });
}

async function typeCommand(page: Page, command: string): Promise<void> {
  const textarea = page.locator('.term-layer[style*="visible"] .ghostty-input');
  await textarea.focus();
  await page.keyboard.type(command);
  await page.keyboard.press("Enter");
}

/** Scans the terminal's public snapshots while stepping through scrollback. */
async function bufferReport(page: Page): Promise<BufferReport> {
  const report = await page.evaluate(async ({ paintTimeoutMs, sampleLimit, rowCharacters }) => {
    interface StageShape {
      layers: Map<string, {
        el: HTMLElement;
        term: {
          cols: number;
          rows: number;
          modes: { synchronizedOutputMode: boolean };
          buffer: { active: { length: number; baseY: number; viewportY: number } };
          renderSnapshot(): { rowData: Array<{ text: string }> } | null;
          scrollToLine(line: number): void;
          onRender(listener: () => void): { dispose(): void };
        };
      }>;
    }
    const stage = (window as unknown as { __pmStage?: StageShape }).__pmStage;
    if (!stage) throw new Error("stage probe missing");
    for (const [key, layer] of stage.layers) {
      if (layer.el.style.visibility !== "visible" || !layer.el.isConnected) continue;
      const buffer = layer.term.buffer.active;
      const savedViewport = buffer.viewportY;
      let firstHistoryLinePresent = false;
      let lastHistoryIndexSeen = -1;
      const historyLines = new Set<number>();
      const samples: Array<object> = [];
      let paints = 0;
      let viewportMismatches = 0;
      const viewportState = () => ({ ...layer.term.buffer.active });
      const socketState = () => Object.entries((window as unknown as ProbeWindow).__claudeProbe.sockets)
        .map(([url, metric]) => ({
          endpoint: url.split("?")[0], bytes: metric.bytes, created: metric.created, closed: metric.closed,
          historyTopReceived: metric.historyTopReceived, events: metric.events.slice(),
        }));
      const socketsBefore = socketState();
      const scrollToPaintedLine = (line: number, restoring = false) => new Promise<void>((resolve, reject) => {
        const beforeRequest = viewportState();
        let afterRequest = beforeRequest;
        const requestedAt = performance.now();
        const render = layer.term.onRender(() => {
          window.clearTimeout(timeout);
          render.dispose();
          paints += 1;
          const atPaint = viewportState();
          const rows = layer.term.renderSnapshot()?.rowData ?? [];
          const indices = rows.flatMap((row) => [...row.text.matchAll(/CLAUDE-HISTORY-(\d{5})/g)]
            .map((match) => Number(match[1])));
          const expectedViewport = Math.max(0, Math.min(afterRequest.baseY, line));
          if (atPaint.viewportY !== expectedViewport) viewportMismatches += 1;
          samples.push({
            requestedLine: line, restoring, requestedAt, paintedAt: performance.now(), beforeRequest,
            afterRequest, atPaint, cols: layer.term.cols, rows: layer.term.rows,
            synchronizedOutput: layer.term.modes.synchronizedOutputMode,
            firstRows: rows.slice(0, 2).map((row) => row.text.slice(0, rowCharacters)),
            lastRows: rows.slice(-2).map((row) => row.text.slice(0, rowCharacters)),
            firstHistoryIndex: indices.length ? Math.min(...indices) : null,
            lastHistoryIndex: indices.length ? Math.max(...indices) : null,
            historyTopPresent: indices.includes(0),
          });
          if (samples.length > sampleLimit) samples.splice(Math.floor(sampleLimit / 2), 1);
          resolve();
        });
        const timeout = window.setTimeout(() => {
          render.dispose();
          reject(new Error(`terminal did not paint history row ${line}`));
        }, paintTimeoutMs);
        layer.term.scrollToLine(line);
        afterRequest = viewportState();
      });
      const step = Math.max(1, layer.term.rows - 1);
      try {
        for (let line = 0; line < buffer.length; line += step) {
          await scrollToPaintedLine(line);
          for (const row of layer.term.renderSnapshot()?.rowData ?? []) {
            const match = row.text.match(/CLAUDE-HISTORY-(\d{5})/);
            if (!match) continue;
            const historyIndex = Number.parseInt(match[1], 10);
            historyLines.add(historyIndex);
            if (historyIndex === 0) firstHistoryLinePresent = true;
            if (historyIndex > lastHistoryIndexSeen) lastHistoryIndexSeen = historyIndex;
          }
        }
      } finally {
        await scrollToPaintedLine(savedViewport, true);
      }
      return {
        bufferLines: buffer.length,
        baseY: buffer.baseY,
        viewportY: buffer.viewportY,
        firstHistoryLinePresent,
        lastHistoryIndexSeen,
        historyLinesInBuffer: historyLines.size,
        diagnostics: {
          key, paints, viewportMismatches, savedViewport, samples, socketsBefore, socketsAfter: socketState(),
          layerVisible: layer.el.style.visibility, connected: layer.el.isConnected,
        },
      };
    }
    throw new Error("no visible layer");
  }, {
    paintTimeoutMs: HISTORY_PAINT_TIMEOUT_MS,
    sampleLimit: HISTORY_SCAN_SAMPLE_LIMIT,
    rowCharacters: HISTORY_SCAN_ROW_CHARACTERS,
  });
  if (!report.firstHistoryLinePresent) {
    const body = JSON.stringify(report, null, 2);
    console.log(`CLAUDE_HISTORY_SCAN_DIAGNOSTIC ${JSON.stringify(report)}`);
    await test.info().attach("history-scan-diagnostic", { body, contentType: "application/json" });
  }
  return report;
}

function transcriptState(report: BufferReport): typeof TRANSCRIPT_RESTORED {
  return {
    firstHistoryLinePresent: report.firstHistoryLinePresent,
    deepScrollback: report.baseY > MIN_TRANSCRIPT_BASE_Y,
  };
}

async function streamContains(page: Page, text: string): Promise<boolean> {
  return page.evaluate((needle) => Object.values((window as unknown as ProbeWindow).__claudeProbe.sockets)
    .some((socket) => socket.textTail.includes(needle)), text);
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

  await expect.poll(() => streamContains(page, STREAM_DONE_MARKER), {
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

  const socketSummary = await page.evaluate(() => (window as unknown as ProbeWindow).__claudeProbe.sockets);
  console.log(`CLAUDE_REPLAY_METRICS ${JSON.stringify({ streaming, returned, resized, socketSummary })}`);
});
