import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

interface SocketRecord {
  url: string;
  closeCalls: number;
  closeEvents: number;
}

type TerminalStageSnapshot = Array<{
  key: string;
  socket: { generation: string; phase: "online" | "reconnecting" } | null;
}>;

const TERMINAL_READY_TIMEOUT_MS = 20_000;

async function installSocketTracker(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const NativeWebSocket = window.WebSocket;
    const records: SocketRecord[] = [];
    class TrackedWebSocket extends NativeWebSocket {
      private record: SocketRecord;

      constructor(url: string | URL, protocols?: string | string[]) {
        if (protocols === undefined) super(url);
        else super(url, protocols);
        this.record = { url: String(url), closeCalls: 0, closeEvents: 0 };
        records.push(this.record);
        this.addEventListener("close", () => {
          this.record.closeEvents += 1;
        });
      }

      override close(code?: number, reason?: string): void {
        this.record.closeCalls += 1;
        super.close(code, reason);
      }
    }
    Object.defineProperty(window, "WebSocket", { configurable: true, value: TrackedWebSocket });
    Object.defineProperty(window, "__pmSocketRecords", { value: records });
  });
}

async function stageSnapshot(page: Page): Promise<TerminalStageSnapshot> {
  return page.evaluate(() => (
    window as Window & { __pmStage?: { debugSnapshot(): TerminalStageSnapshot } }
  ).__pmStage?.debugSnapshot() ?? []);
}

async function terminalSockets(page: Page): Promise<SocketRecord[]> {
  return page.evaluate(() => (
    window as Window & { __pmSocketRecords?: SocketRecord[] }
  ).__pmSocketRecords?.filter((record) => record.url.includes("/ws/terminal/")) ?? []);
}

async function expectTerminalOnline(page: Page, key: string): Promise<void> {
  await expect.poll(async () => (
    await stageSnapshot(page)
  ).find((layer) => layer.key === key)?.socket?.phase, {
    message: `waiting for terminal ${key} to finish replay and accept input`,
    timeout: TERMINAL_READY_TIMEOUT_MS,
  }).toBe("online");
}

async function openSession(page: Page, title: string): Promise<string> {
  await page.locator(".sb-session-title")
    .filter({ hasText: new RegExp(`^${title}$`) })
    .locator("xpath=ancestor::button[contains(@class, 'sb-session')][1]")
    .click();
  await expect(page.locator('.term-layer[style*="visible"]')).toBeVisible();
  const match = page.url().match(/#\/session\/(\d+)/);
  if (!match) throw new Error(`missing session route in ${page.url()}`);
  await expectTerminalOnline(page, `s:${match[1]}`);
  return match[1];
}

async function exitVisibleAgent(page: Page, code: number): Promise<void> {
  const textarea = page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
  await textarea.focus();
  await page.keyboard.type(`exit ${code}`);
  await page.keyboard.press("Enter");
}

async function expectLayerAndSocketReleased(page: Page, key: string, socketUrl: string): Promise<void> {
  await expect.poll(async () => (await stageSnapshot(page)).some((layer) => layer.key === key)).toBe(false);
  // The daemon and SessionChanged event race by design. Under load the daemon
  // may close the native socket before stage disposal calls close() itself;
  // either outcome proves the live socket is gone, while unit coverage verifies
  // that disposal also cancels its reconnect timer.
  await expect.poll(async () => (
    await terminalSockets(page)
  ).find((record) => record.url === socketUrl)).toMatchObject({
    url: socketUrl,
  });
  await expect.poll(async () => {
    const record = (await terminalSockets(page)).find((candidate) => candidate.url === socketUrl);
    return (record?.closeCalls ?? 0) + (record?.closeEvents ?? 0);
  }).toBeGreaterThan(0);
}

test("ended, failed, and removed terminals release cached layers and sockets", async ({ page }) => {
  await installSocketTracker(page);
  await logIn(page);

  const exitedId = await openSession(page, "browser-e2e");
  const exitedLayer = (await stageSnapshot(page)).find((layer) => layer.key === `s:${exitedId}`);
  expect(exitedLayer?.socket?.generation).toBe("1");
  const exitedSocket = (await terminalSockets(page)).at(-1)?.url;
  if (!exitedSocket) throw new Error("missing exited-session terminal socket");
  await exitVisibleAgent(page, 0);
  await expectLayerAndSocketReleased(page, `s:${exitedId}`, exitedSocket);

  // Inspecting durable ended-session metadata must not recreate its live terminal.
  await page.locator(".sb-session-title")
    .filter({ hasText: /^browser-e2e$/ })
    .locator("xpath=ancestor::button[contains(@class, 'sb-session')][1]")
    .click();
  await expect(page.getByRole("button", { name: "restart session" })).toBeVisible();
  expect((await stageSnapshot(page)).some((layer) => layer.key === `s:${exitedId}`)).toBe(false);

  await page.getByRole("button", { name: "restart session" }).click();
  await expect(page.locator('.term-layer[style*="visible"]')).toBeVisible();
  await expect.poll(async () => (
    await stageSnapshot(page)
  ).find((layer) => layer.key === `s:${exitedId}`)?.socket, {
    message: "waiting for the restarted terminal generation to accept input",
    timeout: TERMINAL_READY_TIMEOUT_MS,
  }).toMatchObject({ generation: "2", phase: "online" });
  const resumedLayer = (await stageSnapshot(page)).find((layer) => layer.key === `s:${exitedId}`);
  expect(resumedLayer?.socket?.generation).toBe("2");
  expect((await terminalSockets(page)).at(-1)?.url).not.toBe(exitedSocket);

  // A removed shell uses its terminal cache address and closes its own socket.
  const terminalIdOf = (url: string) => url.match(/\/ws\/terminal\/(\d+)/)?.[1];
  const terminalsBeforeShell = new Set((await terminalSockets(page)).map((record) => terminalIdOf(record.url)));
  await page.getByRole("button", { name: "+ Shell", exact: true }).click();
  await expect(page.getByRole("button", { name: /shell \d+/ })).toBeVisible();
  // The tab paints before the shell's socket opens, so the shell is the first socket for an unseen terminal.
  const shellRecord = async () => (await terminalSockets(page))
    .find((record) => !terminalsBeforeShell.has(terminalIdOf(record.url)));
  await expect.poll(async () => (await shellRecord())?.url).toBeTruthy();
  const shellSocket = (await shellRecord())!.url;
  const shellId = terminalIdOf(shellSocket);
  if (!shellId) throw new Error(`missing terminal id in ${shellSocket}`);
  await expectTerminalOnline(page, `t:${shellId}`);
  const shellInput = page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
  await shellInput.focus();
  await page.keyboard.type("exit");
  await page.keyboard.press("Enter");
  await expectLayerAndSocketReleased(page, `t:${shellId}`, shellSocket);

  const failedId = await openSession(page, "browser-e2e-two");
  const failedSocket = (await terminalSockets(page)).at(-1)?.url;
  if (!failedSocket) throw new Error("missing failed-session terminal socket");
  await exitVisibleAgent(page, 7);
  await expectLayerAndSocketReleased(page, `s:${failedId}`, failedSocket);
});
