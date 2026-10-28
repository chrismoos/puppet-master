import { expect, test, type Page } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";
import { terminalCanvasDiagnostics, terminalCanvasGeometry, watchTerminalPointer } from "./terminalCanvasDriver";
import {
  TERMINAL_FLAG_REPLAY_END,
  TERMINAL_TAG_INPUT,
  TERMINAL_TAG_INPUT_SUBMIT,
  TERMINAL_TAG_OUTPUT,
  TERMINAL_TAG_RESIZE,
} from "@puppet-master/client-core/ws/terminalFrame";

const INPUT_PAYLOAD_OFFSET = 9;
const OUTPUT_PAYLOAD_OFFSET = 10;
const RESIZE_FRAME_BYTES = 13;
const CLIPBOARD_DIAGNOSTIC_EVENTS = 64;

interface EarlyWorkspaceClipboardProbe {
  arm: boolean;
  phase: "pass" | "initial" | "size" | "released";
  initialReplayReceived: boolean;
  readyReceived: boolean;
  pendingSizeEcho: { cols: number; rows: number } | null;
  inputs: string[];
  writes: Array<{ text: string; at: number; outcome: string }>;
  events: Array<Record<string, unknown>>;
  releaseInitial(): void;
  releaseSize(): void;
}

type EarlyWorkspaceWindow = Window & { __earlyWorkspaceClipboard: EarlyWorkspaceClipboardProbe };

async function installEarlyWorkspaceClipboardProbe(page: Page): Promise<void> {
  await page.addInitScript(({
    inputTag, submitTag, outputTag, resizeTag, replayEnd, inputOffset, outputOffset, resizeBytes, eventLimit,
  }) => {
    const held: Array<() => void> = [];
    const probe: EarlyWorkspaceClipboardProbe = {
      arm: false, phase: "pass", initialReplayReceived: false, readyReceived: false, pendingSizeEcho: null,
      inputs: [], writes: [], events: [],
      releaseInitial: () => {
        probe.phase = "size";
        record({ kind: "release-initial" });
        for (const deliver of held.splice(0)) deliver();
      },
      releaseSize: () => {
        probe.phase = "released";
        record({ kind: "release-size" });
        for (const deliver of held.splice(0)) deliver();
      },
    };
    (window as unknown as EarlyWorkspaceWindow).__earlyWorkspaceClipboard = probe;
    const record = (event: Record<string, unknown>) => {
      probe.events.push({ at: performance.now(), phase: probe.phase, ...event });
      if (probe.events.length > eventLimit) probe.events.splice(Math.floor(eventLimit / 2), 1);
    };
    const clipboard = navigator.clipboard;
    if (clipboard) {
      const writeText = clipboard.writeText.bind(clipboard);
      clipboard.writeText = (text) => {
        const call = { text, at: performance.now(), outcome: "pending" };
        probe.writes.push(call);
        record({ kind: "clipboard-write", text });
        return writeText(text).then(() => {
          call.outcome = "copied";
        }, (error: unknown) => {
          call.outcome = String(error);
          throw error;
        });
      };
    }
    const NativeWebSocket = window.WebSocket;
    window.WebSocket = new Proxy(NativeWebSocket, {
      construct(target, args) {
        const socket = Reflect.construct(target, args) as WebSocket;
        const endpoint = new URL(String(args[0]), location.href).pathname;
        if (!endpoint.startsWith("/ws/terminal/") || !probe.arm) return socket;
        probe.arm = false;
        probe.phase = "initial";
        record({ kind: "workspace-socket", endpoint });
        let receiver: WebSocket["onmessage"] = null;
        const decoder = new TextDecoder();
        let outputTail = "";
        Object.defineProperty(socket, "onmessage", {
          configurable: true,
          get: () => receiver,
          set: (next: WebSocket["onmessage"]) => { receiver = next; },
        });
        socket.addEventListener("message", (event) => {
          const view = event.data instanceof ArrayBuffer ? new DataView(event.data) : null;
          const tag = view && view.byteLength > 0 ? view.getUint8(0) : null;
          const flags = tag === outputTag && view && view.byteLength >= outputOffset
            ? view.getUint8(outputOffset - 1) : null;
          const size = tag === resizeTag && view?.byteLength === resizeBytes
            ? { cols: view.getUint16(inputOffset, true), rows: view.getUint16(inputOffset + 2, true) } : null;
          if (flags !== null && (flags & replayEnd) !== 0) probe.initialReplayReceived = true;
          if (probe.phase === "size" && size) probe.pendingSizeEcho = size;
          record({ kind: "receive", tag, flags, bytes: view?.byteLength, size });
          if (flags !== null && event.data instanceof ArrayBuffer) {
            const text = outputTail + decoder.decode(new Uint8Array(event.data, outputOffset), { stream: true });
            probe.readyReceived ||= text.includes("READY");
            if (text.includes("\u001b]52;")) record({ kind: "osc52-output", flags, text: text.slice(-256) });
            outputTail = text.slice(-5);
          }
          const deliver = () => {
            record({ kind: "deliver", tag, flags, size });
            receiver?.call(socket, event);
          };
          if (probe.phase === "initial" || (probe.phase === "size" && size)) held.push(deliver);
          else deliver();
        });
        const send = socket.send.bind(socket);
        socket.send = (data: string | ArrayBufferLike | Blob | ArrayBufferView) => {
          const bytes = data instanceof ArrayBuffer ? new Uint8Array(data)
            : ArrayBuffer.isView(data) ? new Uint8Array(data.buffer, data.byteOffset, data.byteLength) : null;
          const tag = bytes?.[0];
          const text = bytes && (tag === inputTag || tag === submitTag)
            ? new TextDecoder().decode(bytes.subarray(inputOffset)) : null;
          if (text !== null) probe.inputs.push(text);
          record({ kind: "send", tag, text, bytes: bytes?.byteLength });
          send(data as Parameters<typeof send>[0]);
        };
        return socket;
      },
    }) as typeof WebSocket;
  }, {
    inputTag: TERMINAL_TAG_INPUT, submitTag: TERMINAL_TAG_INPUT_SUBMIT, outputTag: TERMINAL_TAG_OUTPUT,
    resizeTag: TERMINAL_TAG_RESIZE, replayEnd: TERMINAL_FLAG_REPLAY_END,
    inputOffset: INPUT_PAYLOAD_OFFSET, outputOffset: OUTPUT_PAYLOAD_OFFSET,
    resizeBytes: RESIZE_FRAME_BYTES, eventLimit: CLIPBOARD_DIAGNOSTIC_EVENTS,
  });
}

async function earlyWorkspaceClipboardState(page: Page) {
  return page.evaluate(() => {
    const probe = (window as unknown as EarlyWorkspaceWindow).__earlyWorkspaceClipboard;
    const terminal = document.querySelector(".workspace-terminal-host .ghostty-terminal");
    const style = terminal ? getComputedStyle(terminal) : null;
    return {
      phase: probe.phase, initialReplayReceived: probe.initialReplayReceived, readyReceived: probe.readyReceived,
      pendingSizeEcho: probe.pendingSizeEcho, inputs: probe.inputs.slice(), writes: probe.writes,
      events: probe.events.slice(),
      grid: { cols: style?.getPropertyValue("--ghostty-grid-cols"), rows: style?.getPropertyValue("--ghostty-grid-rows") },
    };
  });
}

function base64(text: string): string {
  return Buffer.from(text, "utf8").toString("base64");
}

async function openTerminal(page: Page) {
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);
  await expectTerminalRevealed(page, `s:${page.url().match(/#\/session\/(\d+)$/)![1]}`);
  const input = page.locator('.term-layer[style*="visible"] .ghostty-input');
  await input.focus();
  return input;
}

const SESSION_HOST = '.term-layer[style*="visible"]';
const WORKSPACE_HOST = ".workspace-terminal-host";

type StageWindow = Window & {
  __pmStage?: { debugSnapshot(): Array<{ key: string; visible: boolean; screenText: string[] }> };
};

async function sendOsc52(page: Page, data: string, host = SESSION_HOST): Promise<void> {
  const input = page.locator(`${host} .ghostty-input`);
  await input.pressSequentially(`osc52 ${data}`);
  await input.press("Enter");
}

/** Opens a split-pane workspace showing the seeded browser-e2e terminal. */
async function openWorkspacePane(page: Page, beforeAttach?: () => Promise<void>, visitSession = true) {
  await logIn(page);
  if (visitSession) {
    const session = page.locator(".sb-session-title")
      .filter({ hasText: /^browser-e2e$/ })
      .locator("xpath=ancestor::button[contains(@class, 'sb-session')][1]");
    await session.click();
    const sessionId = page.url().match(/#\/session\/(\d+)$/)?.[1];
    if (!sessionId) throw new Error("browser-e2e session did not open");
    await expectTerminalRevealed(page, `s:${sessionId}`);
    await expect.poll(() => page.evaluate((key) => {
      const stage = (window as StageWindow).__pmStage;
      const layer = stage?.debugSnapshot().find((entry) => entry.key === key);
      return layer?.screenText.some((row) => row.includes("READY")) ?? false;
    }, `s:${sessionId}`), {
      message: "browser-e2e agent prompt is rendered before opening the workspace pane",
      timeout: 20_000,
    }).toBe(true);
  }
  await page.getByTitle("new workspace").click();
  await page.getByLabel("name", { exact: true }).fill("clipboard workspace");
  await page.getByRole("button", { name: "create workspace" }).click();
  await beforeAttach?.();
  await page.getByLabel("terminal shown in pane").selectOption({ label: "browser-e2e · agent" });
  await expect(page.locator(`${WORKSPACE_HOST} .ghostty-terminal`)).toBeVisible();
  const input = page.locator(`${WORKSPACE_HOST} .ghostty-input`);
  await input.focus();
  return input;
}


/**
 * Replays Safari's clipboard policy inside Chromium: a write succeeds only
 * when it is issued synchronously from a user gesture. Chromium itself
 * grants gesture-free writes to a focused page once the permission is
 * granted, which the immediate-success test relies on.
 */
async function requireSynchronousGesture(page: Page): Promise<void> {
  await page.addInitScript(() => {
    let inGesture = false;
    for (const type of ["pointerdown", "keydown"]) {
      document.addEventListener(type, () => {
        inGesture = true;
        setTimeout(() => { inGesture = false; }, 0);
      }, { capture: true });
    }
    const clipboard = navigator.clipboard;
    const original = clipboard.writeText.bind(clipboard);
    const calls: Array<{ text: string; inGesture: boolean }> = [];
    const probe = window as typeof window & {
      __clipboardWrites?: typeof calls;
      __clipboardSeed?: (text: string) => Promise<void>;
    };
    probe.__clipboardWrites = calls;
    probe.__clipboardSeed = original;
    clipboard.writeText = (text: string) => {
      calls.push({ text, inGesture });
      if (!inGesture) {
        const error = new Error("Document is not focused.");
        error.name = "NotAllowedError";
        return Promise.reject(error);
      }
      return original(text);
    };
  });
}

async function seedClipboard(page: Page, text: string): Promise<void> {
  await page.evaluate((seed) => (
    (window as typeof window & { __clipboardSeed?: (text: string) => Promise<void> }).__clipboardSeed!(seed)
  ), text);
}

async function clipboardWrites(page: Page): Promise<Array<{ text: string; inGesture: boolean }>> {
  return page.evaluate(() => (
    (window as typeof window & { __clipboardWrites?: Array<{ text: string; inGesture: boolean }> }).__clipboardWrites ?? []
  ));
}

test("an OSC 52 copy the browser accepts reaches the clipboard immediately", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await openTerminal(page);
  const status = page.locator(".terminal-clipboard-status");
  const text = "copied straight from the agent";

  await sendOsc52(page, `c;${base64(text)}`);
  await expect(status).toBeVisible();
  await expect(status).toHaveClass(/is-copied/);
  await expect(status).toContainText(`Copied ${text.length} chars`);
  await expect(status.getByRole("button")).toBeHidden();
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(text);
  await expect(status).toBeHidden({ timeout: 10_000 });
});

test("an OSC 52 copy refused without activation is parked and completes on the next gesture", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await requireSynchronousGesture(page);
  await openTerminal(page);
  await seedClipboard(page, "stale");
  const status = page.locator(".terminal-clipboard-status");
  const text = "parked until a gesture — 日本";

  await sendOsc52(page, `c;${base64(text)}`);
  await expect(status).toBeVisible();
  await expect(status).toHaveClass(/is-pending/);
  await expect(status).toContainText(`Copy of ${text.length} chars is waiting for a click or keypress`);
  await expect(status.getByRole("button", { name: "copy now" })).toBeVisible();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe("stale");
  expect((await clipboardWrites(page)).at(-1)).toEqual({ text, inGesture: false });

  await status.getByRole("button", { name: "copy now" }).click();
  await expect(status).toHaveClass(/is-copied/);
  await expect(status).toContainText(`Copied ${text.length} chars`);
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(text);
  expect((await clipboardWrites(page)).at(-1)).toEqual({ text, inGesture: true });

  const second = "a plain click on the terminal also completes";
  await sendOsc52(page, `c;${base64(second)}`);
  await expect(status).toHaveClass(/is-pending/);
  const terminal = page.locator('.term-layer[style*="visible"] .ghostty-canvas');
  const box = await terminal.boundingBox();
  await page.mouse.click(box!.x + box!.width / 2, box!.y + box!.height / 2);
  await expect(status).toHaveClass(/is-copied/);
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(second);
});

test("an OSC 52 read request is ignored and never touches the clipboard", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await requireSynchronousGesture(page);
  await openTerminal(page);
  await seedClipboard(page, "private");
  const status = page.locator(".terminal-clipboard-status");

  await sendOsc52(page, "c;?");
  await sendOsc52(page, `c;${base64("marker after the read")}`);
  await expect(status).toHaveClass(/is-pending/);
  expect((await clipboardWrites(page)).map((call) => call.text)).toEqual(["marker after the read"]);
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe("private");
});

test("a workspace pane routes an agent OSC 52 copy to the clipboard", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await installEarlyWorkspaceClipboardProbe(page);
  try {
    await openWorkspacePane(page, () => page.evaluate(() => {
      (window as unknown as EarlyWorkspaceWindow).__earlyWorkspaceClipboard.arm = true;
    }), false);
    const status = page.locator(`${WORKSPACE_HOST} .terminal-clipboard-status`);
    const text = "copied from a workspace pane";
    const command = `osc52 c;${base64(text)}`;
    await expect.poll(() => earlyWorkspaceClipboardState(page).then((state) => state.initialReplayReceived && state.readyReceived))
      .toBe(true);

    await sendOsc52(page, `c;${base64(text)}`, WORKSPACE_HOST);
    const beforeReplay = await earlyWorkspaceClipboardState(page);
    expect(beforeReplay.inputs).toEqual([]);
    expect(beforeReplay.writes).toEqual([]);
    await expect(status).toBeHidden();

    await page.evaluate(() => (window as unknown as EarlyWorkspaceWindow).__earlyWorkspaceClipboard.releaseInitial());
    await expect.poll(() => earlyWorkspaceClipboardState(page).then((state) => state.pendingSizeEcho))
      .not.toBeNull();
    const beforeSizeEcho = await earlyWorkspaceClipboardState(page);
    expect(beforeSizeEcho.inputs).toEqual([]);
    expect(beforeSizeEcho.writes).toEqual([]);

    await page.evaluate(() => (window as unknown as EarlyWorkspaceWindow).__earlyWorkspaceClipboard.releaseSize());
    await expect(status).toBeVisible();
    await expect(status).toHaveClass(/is-copied/);
    await expect(status).toContainText(`Copied ${text.length} chars`);
    await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(text);
    const settled = await earlyWorkspaceClipboardState(page);
    expect(settled.inputs.join("")).toBe(`${command}\r`);
    expect(settled.writes.map((call) => ({ text: call.text, outcome: call.outcome })))
      .toEqual([{ text, outcome: "copied" }]);
  } finally {
    const diagnostic = await earlyWorkspaceClipboardState(page);
    console.log(`EARLY_WORKSPACE_CLIPBOARD_DIAGNOSTIC ${JSON.stringify(diagnostic)}`);
    await test.info().attach("early-workspace-clipboard", {
      body: JSON.stringify(diagnostic, null, 2), contentType: "application/json",
    });
  }
});

test("a workspace pane parks a refused OSC 52 copy and completes it on a click", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await requireSynchronousGesture(page);
  await openWorkspacePane(page);
  await seedClipboard(page, "stale");
  const status = page.locator(`${WORKSPACE_HOST} .terminal-clipboard-status`);
  const text = "parked in a workspace pane";

  await sendOsc52(page, `c;${base64(text)}`, WORKSPACE_HOST);
  await expect(status).toHaveClass(/is-pending/);
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe("stale");

  const screen = page.locator(`${WORKSPACE_HOST} .ghostty-canvas`);
  const box = await screen.boundingBox();
  await page.mouse.click(box!.x + box!.width / 2, box!.y + box!.height / 2);
  await expect(status).toHaveClass(/is-copied/);
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(text);
});

test("a workspace pane copies on select and on the copy shortcut", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  const input = await openWorkspacePane(page);
  const terminal = page.locator(`${WORKSPACE_HOST} .ghostty-terminal`);
  await watchTerminalPointer(terminal);
  let lastPoint: { x: number; y: number } | undefined;
  const marker = "WORKSPACE-SELECTION-MARKER";
  await input.pressSequentially(`echo ${marker}`);
  await input.press("Enter");

  await expect.poll(async () => {
    const geometry = await terminalCanvasGeometry(terminal);
    const cursor = await input.boundingBox();
    if (!cursor) throw new Error("workspace terminal input is not mounted");
    const rowY = cursor.y - geometry.cellHeight / 2;
    const startX = geometry.left + geometry.cellWidth * 4.5;
    const endX = geometry.left + geometry.cellWidth * (4 + marker.length - 0.5);
    lastPoint = { x: startX, y: rowY };
    await page.mouse.move(startX, rowY);
    await page.mouse.down();
    await page.mouse.move(endX, rowY, { steps: 6 });
    await page.mouse.up();
    return page.evaluate(() => navigator.clipboard.readText());
  }, { message: "select-to-copy in a workspace pane", timeout: 10_000 }).toBe(marker).catch(async (error: unknown) => {
    const diagnostics = await terminalCanvasDiagnostics(terminal, lastPoint);
    console.error("canvas-selection-diagnostics", JSON.stringify(diagnostics));
    await test.info().attach("terminal-selection-geometry", { body: JSON.stringify(diagnostics, null, 2), contentType: "application/json" });
    throw error;
  });

  await page.evaluate(() => navigator.clipboard.writeText("replaced"));
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe("replaced");
  await page.keyboard.press("Control+Shift+C");
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(marker);
  await expect(page.locator(`${WORKSPACE_HOST} .terminal-clipboard-status`)).toBeHidden();
});

/**
 * A plain-HTTP origin has no navigator.clipboard at all, and its legacy
 * copy command succeeds only inside a user gesture. Automated Chromium
 * accepts the command gesture-free, so the gesture rule is replayed here
 * while an in-gesture copy still runs the real command.
 */
async function removeAsyncClipboard(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const clipboard = navigator.clipboard;
    (window as typeof window & { __readClipboard?: () => Promise<string> }).__readClipboard =
      () => clipboard.readText();
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: undefined });
    let inGesture = false;
    for (const type of ["pointerdown", "keydown"]) {
      document.addEventListener(type, () => {
        inGesture = true;
        setTimeout(() => { inGesture = false; }, 0);
      }, { capture: true });
    }
    const original = document.execCommand.bind(document);
    document.execCommand = (command: string, ...rest: unknown[]) => (
      command === "copy" && !inGesture ? false : (original as (...args: unknown[]) => boolean)(command, ...rest)
    );
  });
}

test("on an origin without navigator.clipboard an OSC 52 copy is parked, explained, and completed by a click", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read"]);
  await removeAsyncClipboard(page);
  await openTerminal(page);
  expect(await page.evaluate(() => navigator.clipboard)).toBeUndefined();
  const readClipboard = () => page.evaluate(() => (
    (window as typeof window & { __readClipboard?: () => Promise<string> }).__readClipboard!()
  ));
  const status = page.locator(".terminal-clipboard-status");
  const text = "copied over plain http";

  await sendOsc52(page, `c;${base64(text)}`);
  await expect(status).toBeVisible();
  await expect(status).toHaveClass(/is-pending/);
  await expect(status).toContainText("not HTTPS or localhost");
  await expect(status.getByRole("button", { name: "copy now" })).toBeVisible();

  await status.getByRole("button", { name: "copy now" }).click();
  await expect(status).toHaveClass(/is-copied/);
  await expect(status).toContainText(`Copied ${text.length} chars`);
  await expect.poll(readClipboard).toBe(text);
});
