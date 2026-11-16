import { execFileSync } from "node:child_process";
import { expect, test, type Page } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";

const ACTIVE_SESSIONS = 11;
const HOT_TERMINALS = 8;
const WARM_BYTES = 64 * 1024 * 1024;
const HISTORY_LINES = 3000;
const MAX_HISTORY_BATCHES = 30;

interface Stage {
  visibleId: string;
  warmStats(): { warmed: number; bytes: number; active: number };
  layers: Map<string, {
    history: { cursor: number; hasMore: boolean } | null;
    historyLoading: boolean;
    term: {
      buffer: { active: { baseY: number; viewportY: number } };
      scrollToLine(line: number): void;
      onRender(listener: () => void): { dispose(): void };
      renderSnapshot(): { rowData: Array<{ text: string }> } | null;
    };
  }>;
}

async function viewport(page: Page) {
  return page.evaluate(() => {
    const stage = (window as unknown as { __pmStage: Stage }).__pmStage;
    const layer = stage.layers.get(stage.visibleId)!;
    return { ...layer.term.buffer.active, history: layer.history, loading: layer.historyLoading,
      text: layer.term.renderSnapshot()?.rowData.map((row) => row.text).join("\n") ?? "" };
  });
}

async function scroll(page: Page, line: number) {
  await page.evaluate((target) => new Promise<void>((resolve) => {
    const stage = (window as unknown as { __pmStage: Stage }).__pmStage;
    const term = stage.layers.get(stage.visibleId)!.term;
    const rendered = term.onRender(() => { rendered.dispose(); resolve(); });
    term.scrollToLine(target);
  }), line);
}

test("all active sessions preload without opening terminal sockets or renderers", async ({ page }) => {
  for (let index = 2; index < ACTIVE_SESSIONS; index += 1) {
    execFileSync(process.env.PM_E2E_PM_BIN!, ["--socket", process.env.PM_E2E_SOCKET!,
      "spawn", "--project", "1", "--agent", "codex", "--title", `history-cache-${index}`]);
  }
  let sockets = 0;
  page.on("websocket", (socket) => { if (socket.url().includes("/ws/terminal/")) sockets += 1; });
  await logIn(page, { minimumSessions: ACTIVE_SESSIONS });
  await expect.poll(() => page.evaluate(() => (window as unknown as { __pmStage: Stage }).__pmStage.warmStats().warmed)).toBe(ACTIVE_SESSIONS);
  expect(sockets).toBe(0);
  await expect(page.locator(".ghostty-canvas")).toHaveCount(0);
  for (let index = 0; index < ACTIVE_SESSIONS; index += 1) {
    await page.locator(".sb-session").nth(index).click();
    await expectTerminalRevealed(page, "s:");
  }
  expect(await page.evaluate(() => (window as unknown as { __pmStage: Stage }).__pmStage.layers.size)).toBe(HOT_TERMINALS);
  expect(await page.evaluate(() => (window as unknown as { __pmStage: Stage }).__pmStage.warmStats().bytes)).toBeLessThanOrEqual(WARM_BYTES);
});

test("older history loads in batches and a reload restores a deep reading position", async ({ page }) => {
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await expectTerminalRevealed(page, "s:");
  await page.locator('.term-layer[style*="visible"] .ghostty-input').focus();
  await page.keyboard.type(`lineout ${HISTORY_LINES}`);
  await page.keyboard.press("Enter");
  await expect.poll(async () => (await viewport(page)).text).toContain("HISTORY-2999");
  await page.reload();
  await expectTerminalRevealed(page, "s:");
  expect((await viewport(page)).baseY).toBeLessThan(HISTORY_LINES / 2);
  for (let batch = 0; batch < MAX_HISTORY_BATCHES; batch += 1) {
    const before = await viewport(page);
    if (!before.history?.hasMore) break;
    await scroll(page, 0);
    await expect.poll(async () => (await viewport(page)).history?.cursor).toBeGreaterThan(before.history.cursor);
  }
  expect((await viewport(page)).history?.hasMore).toBe(false);
  await scroll(page, 0);
  await expect.poll(async () => (await viewport(page)).text).toContain("HISTORY-0000");
  const loaded = await viewport(page);
  await scroll(page, Math.floor(loaded.baseY / 2));
  const marker = (await viewport(page)).text.split("\n")[0]!;
  await page.reload();
  await expectTerminalRevealed(page, "s:");
  await expect.poll(async () => (await viewport(page)).text.split("\n")[0]).toBe(marker);
});
