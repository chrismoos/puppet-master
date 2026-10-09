import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

// A program that answers every resize the way Claude Code does: home the
// cursor, erase each row, and repaint its whole screen at the new size.
// Bash runs the trap only between commands, so its repaint can land up to
// half a second after the resize, later than the controller waits for it.
const REDRAWING_PROGRAM = `
draw() {
  cols=$(tput cols); rows=$(tput lines)
  printf '\\033[H'
  for ((i = 0; i < rows; i++)); do printf '\\033[2K\\033[1B'; done
  printf '\\033[HREDRAW HEADER\\r\\nsecond line\\r\\n'
  rule=$(printf '%*s' $((cols - 2)) '' | tr ' ' '-')
  for i in 1 2 3 4 5 6; do printf '%s\\r\\n' "$rule"; done
  printf 'prompt> '
}
trap draw WINCH
draw
while true; do sleep 0.5; done
`;

type Stage = {
  visibleId: string | null;
  layers: Map<string, { term: { cols: number; rows: number; buffer: { active: {
    length: number;
    getLine(row: number): { translateToString(trimRight?: boolean): string } | undefined;
  } } } }>;
};

/** Lines of the visible terminal, scrollback and screen together. */
async function visibleLines(page: Page): Promise<string[]> {
  return page.evaluate(() => {
    const stage = (window as unknown as { __pmStage?: Stage }).__pmStage;
    const layer = stage?.layers.get(stage.visibleId ?? "");
    if (!layer) return [];
    const buffer = layer.term.buffer.active;
    const lines: string[] = [];
    for (let row = 0; row < buffer.length; row += 1) {
      lines.push(buffer.getLine(row)?.translateToString(true) ?? "");
    }
    return lines;
  });
}

async function headerCopies(page: Page): Promise<number> {
  return (await visibleLines(page)).filter((line) => line.includes("REDRAW HEADER")).length;
}

/** Waits for one header, then checks it stays one once the repaint is in. */
async function expectOneHeader(page: Page, step: string): Promise<void> {
  const describe = async () => `${step}: ${JSON.stringify((await visibleLines(page)).filter(Boolean))}`;
  await expect.poll(() => headerCopies(page), { message: step, timeout: 5_000 }).toBe(1);
  // e2e-real-time-wait: the program repaints up to half a second after a resize, and a stale copy would appear only then
  await page.waitForTimeout(1_200);
  expect(await headerCopies(page), await describe()).toBe(1);
}

function countTerminalSockets(page: Page): Promise<number> {
  return page.evaluate(() => (window as unknown as { __terminalSockets: number }).__terminalSockets);
}

async function trackSockets(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const counted = window as unknown as { __terminalSockets: number };
    counted.__terminalSockets = 0;
    const Native = window.WebSocket;
    window.WebSocket = new Proxy(Native, {
      construct(target, args) {
        if (String(args[0]).includes("/ws/terminal/")) counted.__terminalSockets += 1;
        return Reflect.construct(target, args) as WebSocket;
      },
    });
  });
}

test("a program that repaints on resize leaves one copy of its screen in every viewer", async ({ page, context, isolatedDaemon }) => {
  await trackSockets(page);
  await logIn(page);
  await page.setViewportSize({ width: 1400, height: 900 });
  await page.locator(".sb-session-row", { hasText: "browser-e2e" }).first().click();
  await page.locator(".terminal-tabs button", { hasText: "+ Shell" }).click();
  await expect(page.locator(".terminal-tabs")).toContainText("shell");
  const cwd = isolatedDaemon.session("browser-e2e")!.cwd;
  await writeFile(join(cwd, "redraw.sh"), REDRAWING_PROGRAM);
  // e2e-real-time-wait: the shell has no observable ready signal before its first prompt
  await page.waitForTimeout(1500);
  await page.keyboard.type("bash redraw.sh\n");
  await expect.poll(() => headerCopies(page)).toBe(1);
  const socketsBefore = await countTerminalSockets(page);

  await page.getByRole("button", { name: "info" }).click();
  await expectOneHeader(page, "info pane opened");
  await page.getByRole("button", { name: "info" }).click();
  await expectOneHeader(page, "info pane closed");

  await page.setViewportSize({ width: 1400, height: 600 });
  await expectOneHeader(page, "shorter window");
  await page.setViewportSize({ width: 1400, height: 900 });
  await expectOneHeader(page, "taller window");

  for (const width of [1300, 1200, 1100, 1000, 900]) {
    await page.setViewportSize({ width, height: 900 });
    // e2e-real-time-wait: a drag delivers sizes faster than the program repaints, which is the case under test
    await page.waitForTimeout(30);
  }
  await expectOneHeader(page, "window dragged narrower");
  expect(await countTerminalSockets(page), "resizes are answered on the open socket").toBe(socketsBefore);

  const viewer = await context.newPage();
  await viewer.setViewportSize({ width: 1100, height: 700 });
  // The address names the shell terminal, so the second viewer opens on it.
  await viewer.goto(page.url());
  await expect.poll(() => headerCopies(viewer)).toBe(1);
  await expectOneHeader(page, "first viewer after a second viewer took the size");
  await page.getByRole("button", { name: "info" }).click();
  await expectOneHeader(page, "first viewer resized again");
  await expectOneHeader(viewer, "second viewer following");
  await viewer.close();
});
