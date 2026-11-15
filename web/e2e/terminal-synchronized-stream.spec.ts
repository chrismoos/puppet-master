import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import { expect, test } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";

interface Terminal {
  write(data: string): void;
  renderSnapshot(): { rowData: Array<{ text: string }> };
}

test("an inline resize keeps its painted screen across a detached clear and redraw", async ({ page, isolatedDaemon }) => {
  await logIn(page);
  await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title", { hasText: /^browser-e2e$/ }) }).click();
  await expectTerminalRevealed(page, `s:${isolatedDaemon.session("browser-e2e")!.id}`);
  const result = await page.evaluate(async () => {
    const stage = (window as unknown as { __pmStage: {
      layers: Map<string, { handle: { close(): void }; term: Terminal & {
        cols: number; rows: number; resize(cols: number, rows: number): void;
      } }>;
      visibleId: string;
    } }).__pmStage;
    const layer = stage.layers.get(stage.visibleId)!;
    layer.handle.close();
    const terminal = layer.term;
    const text = () => terminal.renderSnapshot().rowData.map((row) => row.text).join("\n");
    const paint = () => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    terminal.write("\x1b[?2026l\x1b[2J\x1b[HRESIZE BEFORE");
    await paint();
    terminal.resize(terminal.cols - 1, terminal.rows);
    terminal.write("\x1b[r\x1b[0m\x1b[H\x1b[2J\x1b[3J\x1b[H");
    // e2e-real-time-wait: the detached erase must span browser paints before the redraw starts
    await new Promise((resolve) => setTimeout(resolve, 80));
    const afterErase = text();
    terminal.write("\x1b[?2026h\x1b[HRESIZE AFTER");
    await paint();
    const duringRedraw = text();
    terminal.write("\x1b[?2026l");
    await paint();
    const afterRedraw = text();
    terminal.write("\x1b[2J\x1b[H");
    await paint();
    const intentionalClear = text();
    terminal.write("CLEAR TIMEOUT");
    await paint();
    terminal.resize(terminal.cols - 1, terminal.rows);
    terminal.write("\x1b[2J\x1b[H");
    // e2e-real-time-wait: a program that only clears must paint once the bounded resize hold expires
    await new Promise((resolve) => setTimeout(resolve, 600));
    return { afterErase, duringRedraw, afterRedraw, intentionalClear, afterTimeout: text() };
  });
  expect(result.afterErase).toContain("RESIZE BEFORE");
  expect(result.duringRedraw).toContain("RESIZE BEFORE");
  expect(result.duringRedraw).not.toContain("RESIZE AFTER");
  expect(result.afterRedraw).toContain("RESIZE AFTER");
  expect(result.intentionalClear.trim()).toBe("");
  expect(result.afterTimeout.trim()).toBe("");
});

test("completed updates paint while the next synchronized redraw is still arriving", async ({ page, isolatedDaemon }) => {
  await logIn(page);
  await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title", { hasText: /^browser-e2e$/ }) }).click();
  const session = isolatedDaemon.session("browser-e2e")!;
  await expectTerminalRevealed(page, `s:${session.id}`);

  const result = await page.evaluate(async () => {
    const stage = (window as unknown as { __pmStage: {
      layers: Map<string, { term: Terminal; el: HTMLElement }>;
      visibleId: string;
    } }).__pmStage;
    const layer = stage.layers.get(stage.visibleId)!;
    const canvas = layer.el.querySelector<HTMLCanvasElement>(".ghostty-canvas")!;
    const context = canvas.getContext("2d")!;
    const nextPaint = () => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve())));
    const screen = (text: string) => `\x1b[?25l\x1b[2J\x1b[H${text}\x1b[3;1H\x1b[48;2;65;67;78mPROMPT\x1b[0m`;
    const pixels = () => [...context.getImageData(0, 0, canvas.width, canvas.height).data];
    const text = () => layer.term.renderSnapshot().rowData.map((row) => row.text).join("\n");

    layer.term.write(`\x1b[?2026l${screen("COMPLETE")}`);
    await nextPaint();
    const expectedPixels = pixels();
    layer.term.write(screen("PREVIOUS"));
    await nextPaint();

    layer.term.write(`\x1b[?2026h${screen("COMPLETE")}\x1b[?2026l\x1b[?2026h\x1b[2J\x1b[HPARTIAL`);
    await nextPaint();
    const completedText = text();
    const completedPixels = pixels();
    layer.term.write("\r\x1b[2KFINISHED\x1b[?2026l");
    await nextPaint();
    return {
      completedText,
      completedPixelsMatch: completedPixels.every((value, index) => value === expectedPixels[index]),
      finishedText: text(),
    };
  });

  expect(result.completedText).toContain("COMPLETE");
  expect(result.completedText).not.toContain("PARTIAL");
  expect(result.completedPixelsMatch, "the prompt and completed output stay intact on the canvas").toBe(true);
  expect(result.finishedText).toContain("FINISHED");
});

const STREAM_UPDATES = 100;
const STREAM_UPDATE_INTERVAL_MS = 20;

interface StreamSample { text: string; at: number }
interface StreamWindow extends Window { __streamSamples: StreamSample[]; __stopStreamSampling(): void }

test("a fragmented PTY stream paints completed prompts throughout continuous output", async ({ page, isolatedDaemon }, testInfo) => {
  await logIn(page);
  await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title", { hasText: /^browser-e2e$/ }) }).click();
  await page.locator(".terminal-tabs button", { hasText: "+ Shell" }).click();
  await expectTerminalRevealed(page, "t:");
  const program = `import sys, time
sys.stdout.write("\\x1b[?25l\\x1b[3J\\x1b[2J\\x1b[H\\x1b[?2026h")
for number in range(${STREAM_UPDATES}):
    sys.stdout.write(f"\\x1b[Hstream-frame-{number:03}\\x1b[3;1H\\x1b[48;2;65;67;78mSTABLE PROMPT\\x1b[0m\\x1b[?2026l\\x1b[?2026h\\x1b[2J")
    sys.stdout.flush()
    time.sleep(${STREAM_UPDATE_INTERVAL_MS} / 1000)
sys.stdout.write("\\x1b[Hstream-finished\\x1b[3;1HSTABLE PROMPT\\x1b[?2026l")
sys.stdout.flush()
`;
  await writeFile(join(isolatedDaemon.session("browser-e2e")!.cwd, "stream.py"), program);
  await page.evaluate(() => {
    const w = window as unknown as StreamWindow;
    const stage = (window as unknown as { __pmStage: {
      layers: Map<string, { term: Terminal & { onRender(listener: () => void): { dispose(): void } } }>;
      visibleId: string;
    } }).__pmStage;
    const term = stage.layers.get(stage.visibleId)!.term;
    w.__streamSamples = [];
    const subscription = term.onRender(() => {
      w.__streamSamples.push({
        text: term.renderSnapshot().rowData.map((row) => row.text).join("\n"),
        at: performance.now(),
      });
    });
    w.__stopStreamSampling = () => subscription.dispose();
  });
  await page.locator('.term-layer[style*="visibility: visible"] .ghostty-input').focus();
  await page.keyboard.type("python3 -u stream.py\n");
  await expect.poll(() => page.evaluate(() => (window as unknown as StreamWindow).__streamSamples.some((sample) => sample.text.includes("stream-finished"))))
    .toBe(true);
  const samples = await page.evaluate(() => {
    const w = window as unknown as StreamWindow;
    w.__stopStreamSampling();
    return w.__streamSamples;
  });
  const streaming = samples.filter((sample) => /stream-frame-\d{3}/.test(sample.text));
  const frames = [...new Set(streaming.map((sample) => /stream-frame-(\d{3})/.exec(sample.text)![1]))];
  await testInfo.attach("stream-paints", { body: JSON.stringify({ frames, samples }), contentType: "application/json" });
  expect(frames.length, "completed frames paint during the stream, not only after its last update").toBeGreaterThan(STREAM_UPDATES / 4);
  expect(streaming.every((sample) => sample.text.includes("STABLE PROMPT")), "partial prompt erases never paint").toBe(true);
});
