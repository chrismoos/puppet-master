import { expect, test } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";
import { terminalCanvasGeometry } from "./terminalCanvasDriver";

interface LinkTestWindow extends Window {
  __openedTerminalUrls: string[];
  __pmStage: {
    visibleId: string;
    layers: Map<string, {
      term: { renderSnapshot(): { rowData: Array<{ text: string }> } | null };
    }>;
  };
}

test("terminal URL clicks exclude surrounding parentheses and keep balanced URL parentheses", async ({ page }) => {
  await logIn(page);
  await page.locator(".sb-session-title").filter({ hasText: /^browser-e2e$/ })
    .locator("xpath=ancestor::button[contains(@class, 'sb-session')][1]").click();
  const sessionId = page.url().match(/#\/session\/(\d+)$/)?.[1];
  if (!sessionId) throw new Error("the terminal session did not open");
  await expectTerminalRevealed(page, `s:${sessionId}`);
  await page.evaluate(() => {
    const current = window as unknown as LinkTestWindow;
    current.__openedTerminalUrls = [];
    window.open = ((url?: string | URL) => {
      if (url !== undefined) current.__openedTerminalUrls.push(String(url));
      return null;
    }) as typeof window.open;
  });
  const terminal = page.locator(".term-layer .ghostty-terminal").filter({ visible: true });
  const input = terminal.locator(".ghostty-input");
  const base = "https://example.test/path";
  const destinations: string[] = [];
  for (const destination of [base, `${base}_(details)`, `${base}_(nested_(details))`]) {
    await input.focus();
    await input.pressSequentially(`echo (${destination}).`);
    await input.press("Enter");
    const locate = () => page.evaluate((value) => {
      const stage = (window as unknown as LinkTestWindow).__pmStage;
      const rows = stage.layers.get(stage.visibleId)?.term.renderSnapshot()?.rowData ?? [];
      for (let row = rows.length - 1; row >= 0; row -= 1) {
        const column = rows[row]!.text.indexOf(value);
        if (column >= 0) return { row, column };
      }
      return null;
    }, destination);
    await expect.poll(locate).not.toBeNull();
    const position = (await locate())!;
    const geometry = await terminalCanvasGeometry(terminal);
    const y = geometry.top + geometry.cellHeight * (position.row + 0.5);
    const end = geometry.left + geometry.cellWidth * (position.column + destination.length + 0.5);
    await page.mouse.move(end, y);
    await expect(page.locator(".ghostty-link-hover")).toHaveCount(0);
    const last = end - geometry.cellWidth;
    await page.mouse.move(last, y);
    await expect(page.locator(".ghostty-link-hover")).toHaveCount(1);
    await page.keyboard.down("Control");
    await page.mouse.click(last, y);
    await page.keyboard.up("Control");
    destinations.push(destination);
    await expect.poll(() => page.evaluate(() => (window as unknown as LinkTestWindow).__openedTerminalUrls))
      .toEqual(destinations);
  }
});
