import { expect, test, type BrowserContext, type Page } from "./fixtures";
import { logIn } from "./support";

type ResizeProbeWindow = Window & { __resizeProbe: { resize: number } };

/// Counts pm-terminal-v1 resize frames leaving the page, which is the only
/// place a viewer claims the PTY size.
async function installResizeProbe(context: BrowserContext): Promise<void> {
  await context.addInitScript(() => {
    const probe = { resize: 0 };
    (window as ResizeProbeWindow).__resizeProbe = probe;
    const NativeWebSocket = window.WebSocket;
    window.WebSocket = new Proxy(NativeWebSocket, {
      construct(target, args) {
        const socket = Reflect.construct(target, args) as WebSocket;
        if (String(args[0]).includes("/ws/terminal/")) {
          const nativeSend = socket.send.bind(socket);
          socket.send = (data: string | ArrayBufferLike | Blob | ArrayBufferView) => {
            const bytes = data instanceof ArrayBuffer
              ? new Uint8Array(data)
              : ArrayBuffer.isView(data)
                ? new Uint8Array(data.buffer, data.byteOffset, data.byteLength)
                : null;
            // pm-terminal-v1 frame type 3 is resize.
            if (bytes?.[0] === 3) probe.resize += 1;
            nativeSend(data);
          };
        }
        return socket;
      },
    }) as typeof WebSocket;
  });
}

function resizeCount(page: Page): Promise<number> {
  return page.evaluate(() => (window as ResizeProbeWindow).__resizeProbe.resize);
}

const SETTLE_MS = 400;
const PAST_DEBOUNCE_MS = 1500;

test("regaining window focus claims no PTY size the terminal already holds", async ({ page, context }) => {
  await installResizeProbe(context);
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await expect(page.locator(".xterm")).toBeVisible();

  await expect.poll(async () => {
    const before = await resizeCount(page);
    // Cold attach may negotiate its initial size without a resize frame, so
    // the baseline is trustworthy once the count holds still.
    // e2e-real-time-wait: settling a counter that nothing else signals.
    await page.waitForTimeout(SETTLE_MS);
    return before === await resizeCount(page);
  }, { timeout: 15_000 }).toBe(true);
  const baseline = await resizeCount(page);

  await page.evaluate(() => window.dispatchEvent(new Event("focus")));
  // e2e-real-time-wait: the claim is that no frame is sent, which only elapsed real time past the viewer size debounce can establish.
  await page.waitForTimeout(PAST_DEBOUNCE_MS);

  expect(await resizeCount(page)).toBe(baseline);
});
