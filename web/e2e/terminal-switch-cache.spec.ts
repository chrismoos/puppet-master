import { execFileSync } from "node:child_process";
import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";
import {
  TERMINAL_FLAG_REPLAY_START,
  TERMINAL_TAG_OUTPUT,
  TERMINAL_TAG_OWNERSHIP,
  TERMINAL_TAG_RESIZE,
  TERMINAL_TAG_RESIZE_REQUEST,
  TERMINAL_TAG_RESYNC,
} from "@puppet-master/client-core/ws/terminalFrame";

const TERMINAL_CACHE_SIZE = 8;
const OUTPUT_HEADER_BYTES = 10;
const OUTPUT_FLAGS_OFFSET = 9;
const OWNERSHIP_SETTLE_MS = 300;

interface Stream {
  claims: number;
  ownerships: number;
  resyncs: number;
  replays: number;
  opens: number;
  closes: number;
}

interface ProbeWindow extends Window {
  __cacheProbe: { contexts: WebGL2RenderingContext[]; releases: number; streams: Record<string, Stream> };
  __pmStage?: { visibleId: string; layers: Map<string, {
    el: HTMLElement;
    initialReplayPending: boolean;
    replayPainted: boolean;
    webgl: { active: boolean };
    pendingFit: unknown;
    pendingWrites: number;
    swapPending: boolean;
  }> };
}

async function installProbe(page: Page): Promise<void> {
  await page.addInitScript((tags) => {
    const probe = { contexts: [] as WebGL2RenderingContext[], releases: 0, streams: {} as Record<string, Stream> };
    (window as ProbeWindow).__cacheProbe = probe;
    const extensions = new WeakSet<object>();
    const nativeExtension = WebGL2RenderingContext.prototype.getExtension;
    WebGL2RenderingContext.prototype.getExtension = function (name: string) {
      const result = nativeExtension.call(this, name);
      if (name === "WEBGL_lose_context" && result && !extensions.has(result)) {
        extensions.add(result);
        const extension = result as WEBGL_lose_context;
        const nativeLoss = extension.loseContext;
        extension.loseContext = () => { probe.releases += 1; nativeLoss.call(extension); };
      }
      return result;
    };
    const nativeGetContext = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = new Proxy(nativeGetContext, {
      apply(target, receiver, args) {
        const context = Reflect.apply(target, receiver, args);
        if (args[0] === "webgl2" && context && !probe.contexts.includes(context)) probe.contexts.push(context);
        return context;
      },
    });
    window.WebSocket = new Proxy(window.WebSocket, {
      construct(target, args) {
        const socket = Reflect.construct(target, args) as WebSocket;
        const path = new URL(String(args[0]), location.href).pathname;
        if (!path.startsWith("/ws/terminal/")) return socket;
        const stream = probe.streams[path] ??= { claims: 0, ownerships: 0, resyncs: 0, replays: 0, opens: 0, closes: 0 };
        socket.addEventListener("open", () => stream.opens += 1);
        socket.addEventListener("close", () => stream.closes += 1);
        socket.addEventListener("message", (event) => {
          if (!(event.data instanceof ArrayBuffer) || event.data.byteLength < tags.headerBytes) return;
          const frame = new DataView(event.data);
          if (frame.getUint8(0) === tags.ownership) stream.ownerships += 1;
          if (frame.getUint8(0) === tags.output && (frame.getUint8(tags.flagsOffset) & tags.replayStart)) stream.replays += 1;
        });
        const nativeSend = socket.send.bind(socket);
        socket.send = (data: string | ArrayBufferLike | Blob | ArrayBufferView) => {
          const bytes = data instanceof ArrayBuffer ? new Uint8Array(data)
            : ArrayBuffer.isView(data) ? new Uint8Array(data.buffer, data.byteOffset, data.byteLength) : null;
          if (bytes?.length) {
            if (bytes[0] === tags.resize || bytes[0] === tags.resizeRequest) stream.claims += 1;
            if (bytes[0] === tags.resync) stream.resyncs += 1;
          }
          nativeSend(data);
        };
        return socket;
      },
    });
  }, { output: TERMINAL_TAG_OUTPUT, ownership: TERMINAL_TAG_OWNERSHIP, replayStart: TERMINAL_FLAG_REPLAY_START,
    resize: TERMINAL_TAG_RESIZE, resizeRequest: TERMINAL_TAG_RESIZE_REQUEST, resync: TERMINAL_TAG_RESYNC,
    headerBytes: OUTPUT_HEADER_BYTES, flagsOffset: OUTPUT_FLAGS_OFFSET });
}

async function settle(page: Page): Promise<void> {
  await expect.poll(() => page.evaluate(() => {
    const stage = (window as ProbeWindow).__pmStage;
    const sessionId = location.hash.match(/^#\/session\/(\d+)$/)?.[1];
    const layer = stage?.layers.get(stage.visibleId);
    return Boolean(sessionId && stage?.visibleId === `s:${sessionId}` && layer?.replayPainted
      && layer.webgl.active && layer.el.style.visibility === "visible" && !layer.initialReplayPending
      && !layer.pendingFit && !layer.pendingWrites && !layer.swapPending);
  })).toBe(true);
}

test("a same-size warm switch claims ownership without requesting replay", async ({ page }) => {
  await installProbe(page);
  await logIn(page);
  const sessions = page.locator(".sb-session");
  await sessions.nth(0).click();
  await expect(sessions.nth(0)).toHaveClass(/is-selected/);
  await settle(page);
  const firstPath = await page.evaluate(() => Object.keys((window as ProbeWindow).__cacheProbe.streams)[0]);
  await sessions.nth(1).click();
  await expect(sessions.nth(1)).toHaveClass(/is-selected/);
  await settle(page);
  const before = await page.evaluate((path) => ({ ...(window as ProbeWindow).__cacheProbe.streams[path] }), firstPath);
  await sessions.nth(0).click();
  await expect(sessions.nth(0)).toHaveClass(/is-selected/);
  await settle(page);
  await expect.poll(() => page.evaluate((path) => (window as ProbeWindow).__cacheProbe.streams[path].claims, firstPath))
    .toBeGreaterThan(before.claims);
  await expect.poll(() => page.evaluate((path) => (window as ProbeWindow).__cacheProbe.streams[path].ownerships, firstPath))
    .toBeGreaterThan(before.ownerships);
  // e2e-real-time-wait: observe the absence of replay after the ownership claim and resize debounce settle.
  await page.waitForTimeout(OWNERSHIP_SETTLE_MS);
  const after = await page.evaluate((path) => (window as ProbeWindow).__cacheProbe.streams[path], firstPath);
  expect(after.resyncs).toBe(before.resyncs);
  expect(after.replays).toBe(before.replays);
  expect(after.opens).toBe(before.opens);
  expect(after.closes).toBe(before.closes);
});

test("a full cache releases only the session eviction victim after revisits", async ({ page }) => {
  const titles = ["browser-e2e", "browser-e2e-two"];
  for (let index = titles.length; index <= TERMINAL_CACHE_SIZE; index += 1) {
    const title = `cache-session-${index}`;
    execFileSync(process.env.PM_E2E_PM_BIN!, ["--socket", process.env.PM_E2E_SOCKET!,
      "spawn", "--project", "1", "--agent", "codex", "--title", title], { encoding: "utf8" });
    titles.push(title);
  }
  await installProbe(page);
  await logIn(page, { minimumSessions: titles.length });
  const visit = async (title: string) => {
    const row = page.locator(".sb-session").filter({ has: page.locator(".sb-session-title", { hasText: new RegExp(`^${title}$`) }) });
    await row.click();
    await expect(row).toHaveClass(/is-selected/);
    await settle(page);
  };
  for (const title of titles.slice(0, TERMINAL_CACHE_SIZE)) await visit(title);
  await visit(titles[0]);
  await visit(titles[TERMINAL_CACHE_SIZE - 1]);
  expect(await page.evaluate(() => (window as ProbeWindow).__cacheProbe.contexts.filter((gl) => !gl.isContextLost()).length))
    .toBe(TERMINAL_CACHE_SIZE);
  const before = await page.evaluate(() => (window as ProbeWindow).__cacheProbe.releases);
  await visit(titles[TERMINAL_CACHE_SIZE]);
  const observed = await page.evaluate(() => {
    const probe = (window as ProbeWindow).__cacheProbe;
    const visible = document.querySelector('.term-layer[style*="visibility: visible"]');
    return { releases: probe.releases, active: probe.contexts.filter((gl) => !gl.isContextLost()).length,
      selectedGpu: probe.contexts.some((gl) => visible?.contains(gl.canvas as HTMLCanvasElement) && !gl.isContextLost()) };
  });
  expect(observed.releases - before).toBe(1);
  expect(observed.active).toBe(TERMINAL_CACHE_SIZE);
  expect(observed.selectedGpu).toBe(true);
  await visit(titles[0]);
  expect(await page.evaluate(() => (window as ProbeWindow).__cacheProbe.releases)).toBe(observed.releases);
});


test("returning to a warm session resized by another viewer still refreshes its screen", async ({ page, context }) => {
  await installProbe(page);
  await logIn(page);
  const sessions = page.locator(".sb-session");
  await sessions.nth(0).click();
  await expect(sessions.nth(0)).toHaveClass(/is-selected/);
  await settle(page);
  const firstUrl = page.url();
  const path = await page.evaluate(() => Object.keys((window as ProbeWindow).__cacheProbe.streams)[0]);
  await sessions.nth(1).click();
  await expect(sessions.nth(1)).toHaveClass(/is-selected/);
  await settle(page);
  const other = await context.newPage();
  await other.setViewportSize({ width: 700, height: 600 });
  await other.goto(firstUrl);
  await settle(other);
  const before = await page.evaluate((path) => ({ ...(window as ProbeWindow).__cacheProbe.streams[path] }), path);
  await sessions.nth(0).click();
  await expect(sessions.nth(0)).toHaveClass(/is-selected/);
  await settle(page);
  await expect(other.locator(".terminal-size-prompt")).toBeVisible();
  await expect.poll(() => page.evaluate((path) => (window as ProbeWindow).__cacheProbe.streams[path].resyncs, path))
    .toBeGreaterThan(before.resyncs);
  await expect.poll(() => page.evaluate((path) => (window as ProbeWindow).__cacheProbe.streams[path].replays, path))
    .toBeGreaterThan(before.replays);
  await expect(page.locator('.term-layer[style*="visibility: visible"] .terminal-size-prompt')).toBeHidden();
  await other.close();
});
