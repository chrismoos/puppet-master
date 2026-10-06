import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

// Switching back to a warm agent session showed a stale screen. The layer's
// socket never dropped, so nothing replayed and nothing armed a repaint; the
// open edge asserted the size the PTY already held, which sends no SIGWINCH,
// and the full-screen program never redrew over the frames the layer had
// buffered while it was hidden. Only a real browser can show this: it needs
// two live sockets, one of them hidden and still receiving.

// The testagent paces 50ms per chunk, so this is roughly ten seconds of
// output: long enough that it is still arriving while the reader is looking
// at the other session, which is the whole point of the scenario.
const HIDDEN_OUTPUT_BYTES = 800_000;
const HIDDEN_OUTPUT_CHUNK = 4_000;

interface TerminalProbe {
  /** Resize frames this page sent, newest last, per terminal socket. */
  resizes: Record<string, { cols: number; rows: number }[]>;
  /** Stream bytes received, per terminal socket. */
  bytes: Record<string, number>;
}

type ProbeWindow = Window & { __terminalProbe: TerminalProbe };

async function logInWithProbe(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const probe: TerminalProbe = { resizes: {}, bytes: {} };
    (window as ProbeWindow).__terminalProbe = probe;
    const NativeWebSocket = window.WebSocket;
    window.WebSocket = new Proxy(NativeWebSocket, {
      construct(target, args) {
        const socket = Reflect.construct(target, args) as WebSocket;
        const url = String(args[0]);
        if (url.includes("/ws/terminal/")) {
          const key = new URL(url).pathname;
          probe.resizes[key] ??= [];
          probe.bytes[key] ??= 0;
          socket.addEventListener("message", (event) => {
            if (!(event.data instanceof ArrayBuffer) || event.data.byteLength < 10) return;
            if (new DataView(event.data).getUint8(0) !== 1) return;
            probe.bytes[key] += event.data.byteLength - 10;
          });
          const nativeSend = socket.send.bind(socket);
          socket.send = (data: string | ArrayBufferLike | Blob | ArrayBufferView) => {
            const bytes = data instanceof ArrayBuffer
              ? new Uint8Array(data)
              : ArrayBuffer.isView(data)
                ? new Uint8Array(data.buffer, data.byteOffset, data.byteLength)
                : null;
            // pm-terminal-v1: tag 3 is resize, cols at 9 and rows at 11.
            if (bytes && bytes.length === 13 && bytes[0] === 3) {
              const view = new DataView(bytes.buffer, bytes.byteOffset, bytes.byteLength);
              probe.resizes[key].push({
                cols: view.getUint16(9, true),
                rows: view.getUint16(11, true),
              });
            }
            nativeSend(data);
          };
        }
        return socket;
      },
    }) as typeof WebSocket;
  });
  await logIn(page);
}

function probe(page: Page): Promise<TerminalProbe> {
  return page.evaluate(() => (window as ProbeWindow).__terminalProbe);
}

/** The socket carrying the most stream bytes, which is the session that has
 * been producing output. */
async function busiestTerminal(page: Page): Promise<string> {
  const seen = await probe(page);
  const entries = Object.entries(seen.bytes).sort((a, b) => b[1] - a[1]);
  expect(entries.length, "a terminal socket is open").toBeGreaterThan(0);
  return entries[0][0];
}

test("switching back to a warm session displays without a resize kick", async ({ page }) => {
  await logInWithProbe(page);
  const sessions = page.locator(".sb-session");
  await expect(sessions.nth(1)).toBeVisible();

  await sessions.nth(0).click();
  await expect(page.locator('.term-layer[style*="visible"] .xterm')).toBeVisible();

  // Paced output keeps arriving after the reader looks away, which is what
  // leaves a hidden layer holding frames its program never laid out for.
  const textarea = page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
  await textarea.focus();
  await page.keyboard.type(`pacedout ${HIDDEN_OUTPUT_BYTES} ${HIDDEN_OUTPUT_CHUNK}`);
  await page.keyboard.press("Enter");

  const busy = await busiestTerminal(page);
  const beforeSwitch = (await probe(page)).bytes[busy];

  await sessions.nth(1).click();
  await expect(sessions.nth(1)).toHaveClass(/is-selected/);

  // It has to actually take output while hidden, or there is nothing to
  // repaint and the assert-only path is correct.
  await expect
    .poll(async () => (await probe(page)).bytes[busy] - beforeSwitch, { timeout: 20_000 })
    .toBeGreaterThan(0);
  const beforeReturn = (await probe(page)).resizes[busy].length;

  await sessions.nth(0).click();
  await expect(sessions.nth(0)).toHaveClass(/is-selected/);

  // Switching back must assert the terminal size without a row jiggle cycle,
  // preventing full-screen applications from clearing their screen.
  // e2e-real-time-wait: allow post-switch viewer debounce to settle
  await page.waitForTimeout(150);
  const observed = (await probe(page)).resizes[busy].slice(beforeReturn);
  if (observed.length > 0) {
    const settled = observed[observed.length - 1];
    expect(
      observed.some((size) => size.rows !== settled.rows),
      `expected no row jiggle cycle on switch-back, got ${JSON.stringify(observed)}`,
    ).toBe(false);
  }
  await expect(page.locator('.term-layer[style*="visible"] .xterm')).toBeVisible();
});

test("cold session attach negotiates size on connect without redundant resize frames", async ({ page }) => {
  await logInWithProbe(page);
  const sessions = page.locator(".sb-session");
  await expect(sessions.nth(0)).toBeVisible();

  await sessions.nth(0).click();
  await expect(page.locator('.term-layer[style*="visible"] .xterm')).toBeVisible();

  // e2e-real-time-wait: allow post-attach viewer debounce to settle
  await page.waitForTimeout(200);

  const resizes = (await probe(page)).resizes;
  const entries = Object.values(resizes).flat();
  if (entries.length > 0) {
    const settled = entries[entries.length - 1];
    expect(
      entries.some((size) => size.cols !== settled.cols || size.rows !== settled.rows),
      `expected at most one size assert after attach, got ${JSON.stringify(entries)}`,
    ).toBe(false);
    expect(entries).toHaveLength(1);
  }
});
