import { expect, test, type BrowserContext, type Page } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";

interface ResizeValue {
  cols: number;
  rows: number;
}

interface SocketProbe {
  sent: ResizeValue[];
  echoed: ResizeValue[];
}

type ProbeWindow = Window & {
  __workspaceResizeProbe: { opened: string[]; sockets: Record<string, SocketProbe> };
};

const TERMINAL_TAG_RESIZE = 3;
const TERMINAL_TAG_RESIZE_REQUEST = 8;
const TERMINAL_RESIZE_FRAME_BYTES = 13;
const TERMINAL_RESIZE_REQUEST_FRAME_BYTES = 21;
const TERMINAL_RESIZE_COLS_OFFSET = 9;
const TERMINAL_RESIZE_REQUEST_COLS_OFFSET = 17;
const RESIZE_SEND_DELAY_MS = 800;

async function installResizeProbe(page: Page): Promise<void> {
  await page.addInitScript(({
    resizeTag,
    resizeRequestTag,
    resizeFrameBytes,
    resizeRequestFrameBytes,
    resizeColsOffset,
    resizeRequestColsOffset,
    sendDelayMs,
  }) => {
    const probe: ProbeWindow["__workspaceResizeProbe"] = { opened: [], sockets: {} };
    (window as unknown as ProbeWindow).__workspaceResizeProbe = probe;
    const NativeWebSocket = window.WebSocket;
    window.WebSocket = new Proxy(NativeWebSocket, {
      construct(target, args) {
        const socket = Reflect.construct(target, args) as WebSocket;
        const url = String(args[0]);
        if (!url.includes("/ws/terminal/")) return socket;
        const metrics = probe.sockets[url] ??= { sent: [], echoed: [] };
        probe.opened.push(url);
        socket.addEventListener("message", (event) => {
          if (!(event.data instanceof ArrayBuffer) || event.data.byteLength !== resizeFrameBytes) return;
          const view = new DataView(event.data);
          if (view.getUint8(0) !== resizeTag) return;
          metrics.echoed.push({ cols: view.getUint16(9, true), rows: view.getUint16(11, true) });
        });
        const nativeSend = socket.send.bind(socket);
        socket.send = (data: string | ArrayBufferLike | Blob | ArrayBufferView) => {
          const bytes = data instanceof ArrayBuffer
            ? new Uint8Array(data)
            : ArrayBuffer.isView(data)
              ? new Uint8Array(data.buffer, data.byteOffset, data.byteLength)
              : null;
          if (bytes && ((bytes.byteLength === resizeFrameBytes && bytes[0] === resizeTag)
            || (bytes.byteLength === resizeRequestFrameBytes && bytes[0] === resizeRequestTag))) {
            const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
            const offset = bytes.byteLength === resizeFrameBytes
              ? resizeColsOffset
              : resizeRequestColsOffset;
            metrics.sent.push({ cols: view.getUint16(offset, true), rows: view.getUint16(offset + 2, true) });
          }
          if (bytes && ((bytes.byteLength === resizeFrameBytes && bytes[0] === resizeTag)
            || (bytes.byteLength === resizeRequestFrameBytes && bytes[0] === resizeRequestTag))) {
            setTimeout(() => nativeSend(data as Parameters<typeof nativeSend>[0]), sendDelayMs);
            return;
          }
          nativeSend(data as Parameters<typeof nativeSend>[0]);
        };
        return socket;
      },
    }) as typeof WebSocket;
  }, {
    resizeTag: TERMINAL_TAG_RESIZE,
    resizeRequestTag: TERMINAL_TAG_RESIZE_REQUEST,
    resizeFrameBytes: TERMINAL_RESIZE_FRAME_BYTES,
    resizeRequestFrameBytes: TERMINAL_RESIZE_REQUEST_FRAME_BYTES,
    resizeColsOffset: TERMINAL_RESIZE_COLS_OFFSET,
    resizeRequestColsOffset: TERMINAL_RESIZE_REQUEST_COLS_OFFSET,
    sendDelayMs: RESIZE_SEND_DELAY_MS,
  });
}

async function measuredWorkspaceGrid(page: Page): Promise<ResizeValue> {
  return page.locator(".workspace-terminal-host .ghostty-terminal").evaluate((terminal) => {
    const host = terminal.parentElement;
    if (!host) throw new Error("workspace terminal is detached from its host");
    const style = getComputedStyle(terminal);
    const cellWidth = Number.parseFloat(style.getPropertyValue("--ghostty-cell-width"));
    const cellHeight = Number.parseFloat(style.getPropertyValue("--ghostty-cell-height"));
    const padding = Number.parseFloat(style.getPropertyValue("--ghostty-content-padding"));
    return {
      cols: Math.max(1, Math.floor((host.clientWidth - padding * 2) / cellWidth)),
      rows: Math.max(1, Math.floor((host.clientHeight - padding * 2) / cellHeight)),
    };
  });
}

async function workspaceRendererGrid(page: Page): Promise<ResizeValue | null> {
  return page.locator(".workspace-terminal-host .ghostty-terminal").evaluate((terminal) => {
    const style = getComputedStyle(terminal);
    const cols = Number.parseInt(style.getPropertyValue("--ghostty-grid-cols"), 10);
    const rows = Number.parseInt(style.getPropertyValue("--ghostty-grid-rows"), 10);
    if (!Number.isFinite(cols) || !Number.isFinite(rows)) return null;
    return { cols, rows };
  });
}

async function measuredSessionGrid(page: Page): Promise<ResizeValue> {
  return page.locator('.term-layer[style*="visibility: visible"] .ghostty-terminal').evaluate((terminal) => {
    const host = terminal.parentElement;
    if (!host) throw new Error("session terminal is detached from its host");
    const style = getComputedStyle(terminal);
    const cellWidth = Number.parseFloat(style.getPropertyValue("--ghostty-cell-width"));
    const cellHeight = Number.parseFloat(style.getPropertyValue("--ghostty-cell-height"));
    const padding = Number.parseFloat(style.getPropertyValue("--ghostty-content-padding"));
    return {
      cols: Math.max(1, Math.floor((host.clientWidth - padding * 2) / cellWidth)),
      rows: Math.max(1, Math.floor((host.clientHeight - padding * 2) / cellHeight)),
    };
  });
}

async function openSecondSessionViewer(context: BrowserContext, url: string): Promise<Page> {
  const other = await context.newPage();
  await other.setViewportSize({ width: 700, height: 600 });
  await other.goto(url);
  await expectTerminalRevealed(other, "s:");
  return other;
}

test("workspace terminal holds its grid until the echo and follows another viewer", async ({ page, context }) => {
  await installResizeProbe(page);
  await logIn(page);
  const session = page.locator(".sb-session-title")
    .filter({ hasText: /^browser-e2e$/ })
    .locator("xpath=ancestor::button[contains(@class, 'sb-session')][1]");
  await session.click();
  await expectTerminalRevealed(page, "s:");
  const sessionUrl = page.url();
  const initialSocketCount = await page.evaluate(() => (window as unknown as ProbeWindow).__workspaceResizeProbe.opened.length);

  await page.getByTitle("new workspace").click();
  await page.getByRole("button", { name: "create workspace" }).click();
  await page.getByLabel("terminal shown in pane").first().selectOption({ label: "browser-e2e · agent" });
  const terminal = page.locator(".workspace-terminal-host .ghostty-terminal");
  await expect(terminal).toBeVisible();
  await expect(terminal.locator(".ghostty-input")).toBeAttached();
  await expect.poll(() => page.evaluate(() => (window as unknown as ProbeWindow).__workspaceResizeProbe.opened.length))
    .toBeGreaterThan(initialSocketCount);
  const socketUrl = await page.evaluate((index) =>
    (window as unknown as ProbeWindow).__workspaceResizeProbe.opened[index], initialSocketCount);
  const previousGrid = await measuredWorkspaceGrid(page);
  const previousRendererGrid = await workspaceRendererGrid(page);
  if (!previousRendererGrid) throw new Error("workspace terminal grid metrics are unavailable");
  const sentStart = await page.evaluate((url) =>
    (window as unknown as ProbeWindow).__workspaceResizeProbe.sockets[url]?.sent.length ?? 0, socketUrl);
  const echoStart = await page.evaluate((url) =>
    (window as unknown as ProbeWindow).__workspaceResizeProbe.sockets[url]?.echoed.length ?? 0, socketUrl);

  await page.setViewportSize({ width: 1_740, height: 1_050 });
  await expect.poll(() => measuredWorkspaceGrid(page)).not.toEqual(previousGrid);
  const desired = await measuredWorkspaceGrid(page);
  await expect.poll(() => page.evaluate(({ url, start, size }) =>
    (window as unknown as ProbeWindow).__workspaceResizeProbe.sockets[url]?.sent
      .slice(start).some((entry) => entry.cols === size.cols && entry.rows === size.rows) ?? false,
  { url: socketUrl, start: sentStart, size: desired })).toBe(true);
  expect(await workspaceRendererGrid(page)).toEqual(previousRendererGrid);
  await expect.poll(() => page.evaluate(({ url, size, start }) =>
    (window as unknown as ProbeWindow).__workspaceResizeProbe.sockets[url]?.echoed
      .slice(start).some((echo) => echo.cols === size.cols && echo.rows === size.rows) ?? false,
  { url: socketUrl, size: desired, start: echoStart })).toBe(true);
  await expect.poll(() => workspaceRendererGrid(page)).toEqual(desired);

  const remoteEchoStart = await page.evaluate((url) =>
    (window as unknown as ProbeWindow).__workspaceResizeProbe.sockets[url]?.echoed.length ?? 0, socketUrl);
  const other = await openSecondSessionViewer(context, sessionUrl);
  const remoteSize = await measuredSessionGrid(other);
  await expect.poll(() => page.evaluate(({ url, size, start }) =>
    (window as unknown as ProbeWindow).__workspaceResizeProbe.sockets[url]?.echoed
      .slice(start).some((echo) => echo.cols === size.cols && echo.rows === size.rows) ?? false,
  { url: socketUrl, size: remoteSize, start: remoteEchoStart })).toBe(true);
  await expect.poll(() => workspaceRendererGrid(page)).toEqual(remoteSize);

  await other.close();
});
