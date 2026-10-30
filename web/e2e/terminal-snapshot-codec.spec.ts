import { expect, test } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";
import {
  TERMINAL_FLAG_REPLAY, TERMINAL_FLAG_REPLAY_END, TERMINAL_FLAG_REPLAY_GHOSTTY,
  TERMINAL_FLAG_REPLAY_START, TERMINAL_TAG_OUTPUT,
} from "@puppet-master/client-core/ws/terminalFrame";

const OUTPUT_HEADER_BYTES = 10;
const OUTPUT_FLAGS_OFFSET = 9;
const SNAPSHOT_MAGIC = "GHOSTSNP";

test("browser restores native binary snapshots on attach and reload", async ({ page }) => {
  const snapshots: Buffer[] = [];
  const identities: string[] = [];
  page.on("websocket", (socket) => {
    if (!socket.url().includes("/ws/terminal/")) return;
    identities.push(new URL(socket.url()).searchParams.get("ghostty") ?? "");
    let chunks: Buffer[] = [];
    socket.on("framereceived", ({ payload }) => {
      if (typeof payload === "string" || payload.length < OUTPUT_HEADER_BYTES || payload[0] !== TERMINAL_TAG_OUTPUT) return;
      const flags = payload[OUTPUT_FLAGS_OFFSET]!;
      if (!(flags & TERMINAL_FLAG_REPLAY)) return;
      expect(flags & TERMINAL_FLAG_REPLAY_GHOSTTY).not.toBe(0);
      if (flags & TERMINAL_FLAG_REPLAY_START) chunks = [];
      chunks.push(payload.subarray(OUTPUT_HEADER_BYTES));
      if (flags & TERMINAL_FLAG_REPLAY_END) snapshots.push(Buffer.concat(chunks));
    });
  });
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await expectTerminalRevealed(page, "s:");
  await expect.poll(() => snapshots.length).toBeGreaterThan(0);
  expect(snapshots[0]!.subarray(0, SNAPSHOT_MAGIC.length).toString()).toBe(SNAPSHOT_MAGIC);
  expect(identities.every((identity) => /^ghostty-vt:[a-f0-9]{40}:pm-v1$/.test(identity))).toBe(true);
  const before = snapshots.length;
  await page.reload();
  await page.locator(".sb-session").first().click();
  await expectTerminalRevealed(page, "s:");
  await expect.poll(() => snapshots.length).toBeGreaterThan(before);
});

test("browser explains an incompatible engine before opening a terminal socket", async ({ page }) => {
  await page.route("**/api/terminals/*/snapshot-compatibility?*", (route) => route.fulfill({
    status: 409, body: "Terminal engine changed. Reload this page to update Ghostty.",
  }));
  let terminalSockets = 0;
  page.on("websocket", (socket) => { if (socket.url().includes("/ws/terminal/")) terminalSockets += 1; });
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await expect(page.locator(".terminal-stream-status")).toContainText("Reload this page");
  expect(terminalSockets).toBe(0);
});
