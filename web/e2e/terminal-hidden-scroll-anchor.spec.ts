import { expect, test, type Page } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";

interface TerminalDebug {
  key: string;
  visible: boolean;
  baseY: number;
  viewportY: number;
  screenText: string[];
}

type ProbeWindow = Window & {
  __pmStage?: { debugSnapshot(): TerminalDebug[] };
  __hiddenTerminalOutputBytes: number;
};

async function visibleState(page: Page): Promise<TerminalDebug | null> {
  return page.evaluate(() => (window as unknown as ProbeWindow).__pmStage?.debugSnapshot().find((entry) => entry.visible) ?? null);
}

test("warm hidden output preserves the visible text of a scrolled reader", async ({ page }) => {
  await page.addInitScript(() => {
    const probe = window as unknown as ProbeWindow;
    probe.__hiddenTerminalOutputBytes = 0;
    const NativeWebSocket = window.WebSocket;
    window.WebSocket = new Proxy(NativeWebSocket, {
      construct(target, args) {
        const socket = Reflect.construct(target, args) as WebSocket;
        if (!String(args[0]).includes("/ws/terminal/")) return socket;
        socket.addEventListener("message", (event) => {
          if (!(event.data instanceof ArrayBuffer) || event.data.byteLength < 10) return;
          if (new DataView(event.data).getUint8(0) === 1) {
            probe.__hiddenTerminalOutputBytes += event.data.byteLength - 10;
          }
        });
        return socket;
      },
    }) as typeof WebSocket;
  });
  await logIn(page, { minimumSessions: 2 });
  const sessions = page.locator(".sb-session");
  await sessions.nth(0).click();
  await expectTerminalRevealed(page, "s:");

  const terminal = page.locator('.term-layer[style*="visibility: visible"]');
  const canvas = terminal.locator(".ghostty-canvas");
  await terminal.locator(".ghostty-input").focus();
  await page.keyboard.type("syncout 700 0");
  await page.keyboard.press("Enter");
  await expect.poll(async () => (await visibleState(page))?.baseY ?? 0, { timeout: 20_000 }).toBeGreaterThan(100);
  const bounds = await canvas.boundingBox();
  if (!bounds) throw new Error("visible terminal canvas has no bounds");
  await page.mouse.move(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2);
  await page.mouse.wheel(0, -1_200);
  await expect.poll(async () => {
    const state = await visibleState(page);
    return state ? state.baseY - state.viewportY : 0;
  }).toBeGreaterThan(10);
  const before = await visibleState(page);
  expect(before?.screenText.join("\n")).toMatch(/CODEX-SYNC-\d+/);
  if (!before) throw new Error("visible terminal debug state is missing");

  const byteMark = await page.evaluate(() => (window as unknown as ProbeWindow).__hiddenTerminalOutputBytes);
  await terminal.locator(".ghostty-input").focus();
  await page.keyboard.type("syncout 400 25");
  await page.keyboard.press("Enter");
  await sessions.nth(1).click();
  await expect(sessions.nth(1)).toHaveClass(/is-selected/);
  await expect.poll(async () => page.evaluate((key) => {
    const stage = (window as unknown as ProbeWindow).__pmStage;
    return stage?.debugSnapshot().find((entry) => entry.key === key)?.visible ?? true;
  }, before.key)).toBe(false);
  await expect.poll(() => page.evaluate(() => (window as unknown as ProbeWindow).__hiddenTerminalOutputBytes), {
    timeout: 20_000,
  }).toBeGreaterThan(byteMark + 500);

  await sessions.nth(0).click();
  await expect(sessions.nth(0)).toHaveClass(/is-selected/);
  await expectTerminalRevealed(page, "s:");
  await expect.poll(async () => page.evaluate((key) => {
    const state = (window as unknown as ProbeWindow).__pmStage?.debugSnapshot().find((entry) => entry.key === key);
    return state?.visible ? state.screenText : null;
  }, before.key)).toEqual(before.screenText);
});
