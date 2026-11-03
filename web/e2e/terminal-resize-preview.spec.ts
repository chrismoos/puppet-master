import { writeFile } from "node:fs/promises";
import { join } from "node:path";
import { expect, test, type Page } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";

// An input box the way Claude Code draws one: borders exactly as wide as the
// terminal, repainted at the new width on every resize. The width comes from
// the PTY, since tput prefers a COLUMNS the shell exported at an older size.
const BOXED_PROGRAM = `
draw() {
  cols=$(stty size | cut -d' ' -f2)
  rule=$(printf '%*s' "$cols" '' | tr ' ' '=')
  printf '\\033[H\\033[2J'
  printf 'BOX HEADER\\r\\n%s\\r\\n> input\\r\\n%s' "$rule" "$rule"
}
trap draw WINCH
draw
while true; do sleep 0.02; done
`;

type Grid = { cols: number; rows: number };
type Stage = {
  visibleId: string | null;
  layers: Map<string, {
    pendingFit: Grid | null;
    handle: { ptySize(): Grid | null } | null;
    term: {
      cols: number;
      cellSize: Grid | null;
      renderSnapshot(): (Grid & { rowData: Array<{ text: string }> }) | null;
    };
  }>;
};
type Sample = { drawn: number; pending: number | null; borderRows: number };

/** Records, once per frame, the drawn grid against the size still awaiting its snapshot. */
async function sampleFrames(page: Page): Promise<void> {
  await page.evaluate(() => {
    const target = window as unknown as { __pmStage?: Stage; __resizeSamples: Sample[] };
    const samples: Sample[] = [];
    target.__resizeSamples = samples;
    const tick = () => {
      const stage = target.__pmStage;
      const layer = stage?.layers.get(stage.visibleId ?? "");
      const drawn = layer?.term.renderSnapshot();
      if (layer && drawn) {
        samples.push({
          drawn: drawn.cols,
          pending: layer.pendingFit?.cols ?? null,
          borderRows: drawn.rowData.filter((row) => row.text.includes("=")).length,
        });
      }
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });
}

function samples(page: Page): Promise<Sample[]> {
  return page.evaluate(() => (window as unknown as { __resizeSamples: Sample[] }).__resizeSamples);
}

function settled(page: Page): Promise<boolean> {
  return page.evaluate(() => {
    const stage = (window as unknown as { __pmStage?: Stage }).__pmStage;
    const layer = stage?.layers.get(stage.visibleId ?? "");
    if (!layer || layer.pendingFit) return false;
    const drawn = layer.term.renderSnapshot();
    return drawn?.cols === layer.term.cellSize?.cols && drawn?.cols === layer.term.cols
      && drawn?.cols === layer.handle?.ptySize()?.cols;
  });
}

async function drag(page: Page, from: number, to: number): Promise<void> {
  const step = from < to ? 25 : -25;
  for (let width = from; step > 0 ? width <= to : width >= to; width += step) {
    await page.setViewportSize({ width, height: 900 });
  }
  await expect.poll(() => settled(page)).toBe(true);
}

test("a dragged window reflows its terminal before the snapshot at the new size arrives", async ({ page, isolatedDaemon }) => {
  await page.setViewportSize({ width: 900, height: 900 });
  await logIn(page);
  await page.locator(".sb-session-row", { hasText: "browser-e2e" }).first().click();
  await page.locator(".terminal-tabs button", { hasText: "+ Shell" }).click();
  await expect(page.locator(".terminal-tabs")).toContainText("shell");
  const cwd = isolatedDaemon.session("browser-e2e")!.cwd;
  await writeFile(join(cwd, "boxed.sh"), BOXED_PROGRAM);
  await expectTerminalRevealed(page, "t:");
  await expect.poll(() => settled(page)).toBe(true);
  await page.locator('.term-layer[style*="visibility: visible"] .ghostty-input').focus();
  await page.keyboard.type("bash boxed.sh\n");
  await expect.poll(async () => (await samplesNow(page)).borderRows).toBe(2);
  await expect.poll(() => settled(page)).toBe(true);
  await sampleFrames(page);

  await drag(page, 900, 1400);
  const widening = (await samples(page)).filter((sample) => sample.pending !== null);
  expect(widening.length, "the drag was answered before a frame could be drawn").toBeGreaterThan(0);
  expect(widening.filter((sample) => sample.drawn === sample.pending).length, JSON.stringify(widening))
    .toBeGreaterThan(0);

  await sampleFrames(page);
  await drag(page, 1400, 900);
  const narrowing = await samples(page);
  expect(narrowing.filter((sample) => sample.pending !== null).length).toBeGreaterThan(0);
  expect(narrowing.map((sample) => sample.borderRows).filter((rows) => rows !== 2), JSON.stringify(narrowing))
    .toEqual([]);
});

function samplesNow(page: Page): Promise<{ borderRows: number }> {
  return page.evaluate(() => {
    const stage = (window as unknown as { __pmStage?: Stage }).__pmStage;
    const drawn = stage?.layers.get(stage.visibleId ?? "")?.term.renderSnapshot();
    return { borderRows: drawn?.rowData.filter((row) => row.text.includes("=")).length ?? 0 };
  });
}

test("content with scrollback holds still while the window height is dragged", async ({ page }) => {
  await page.setViewportSize({ width: 1200, height: 900 });
  await logIn(page);
  await page.locator(".sb-session-row", { hasText: "browser-e2e" }).first().click();
  await page.locator(".terminal-tabs button", { hasText: "+ Shell" }).click();
  await expect(page.locator(".terminal-tabs")).toContainText("shell");
  await expectTerminalRevealed(page, "t:");
  await expect.poll(() => settled(page)).toBe(true);
  await page.locator('.term-layer[style*="visibility: visible"] .ghostty-input').focus();
  await page.keyboard.type("seq 1 300\n");
  await expect.poll(() => page.evaluate(() => {
    const stage = (window as unknown as { __pmStage?: Stage }).__pmStage;
    return stage?.layers.get(stage.visibleId ?? "")?.term.renderSnapshot()?.rowData.some((row) => row.text === "300");
  })).toBe(true);
  await page.evaluate(() => {
    const target = window as unknown as { __origins: string[] };
    target.__origins = [];
    const terminal = document.querySelector('.term-layer[style*="visibility: visible"] .ghostty-terminal')
      ?? document.querySelector('.term-layer[style*="visibility: visible"] [style*="--ghostty-content-origin-y"]');
    const tick = () => {
      if (terminal) target.__origins.push(getComputedStyle(terminal).getPropertyValue("--ghostty-content-origin-y"));
      requestAnimationFrame(tick);
    };
    requestAnimationFrame(tick);
  });

  for (let height = 900; height >= 650; height -= 7) await page.setViewportSize({ width: 1200, height });
  for (let height = 650; height <= 900; height += 7) await page.setViewportSize({ width: 1200, height });
  const origins = await page.evaluate(() => (window as unknown as { __origins: string[] }).__origins.map(parseFloat));
  expect(origins.length).toBeGreaterThan(0);
  // Bottom-anchored content would step down a pixel at a time and jump back
  // up a row as rows come and go. Held content only ever moves up, to stay inside.
  const downward = origins.filter((origin, index) => index > 0 && origin > origins[index - 1]!);
  expect(downward, JSON.stringify(origins)).toEqual([]);
});
