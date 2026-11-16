import { readFile } from "node:fs/promises";
import { expect, test, type Page } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";

interface Grid { cols: number; rows: number }
interface Frame { data: Uint8Array; replay: boolean; snapshot?: boolean; ghosttySnapshot?: boolean }
interface Layer {
  key: string;
  el: HTMLElement;
  curtainEl: HTMLElement;
  initialReplayPending: boolean;
  term: {
    cellSize: Grid | null;
    renderSnapshot(): (Grid & { rowData: Array<{ text: string }> }) | null;
  };
}
interface Stage {
  debugSnapshot(): Array<{ key: string; socket: { replayCount: number } | null }>;
  visibleId: string | null;
  layers: Map<string, Layer>;
  receiveFrame(key: string, frame: Frame): void;
  disposeSession(id: bigint): void;
}
interface Sample {
  selected: string | null;
  shown: Array<{ key: string; painted: Grid; measured: Grid; text: string }>;
}
interface ProbeWindow extends Window {
  __pmStage?: Stage;
  __geometrySamples: Sample[];
  __holdTerminal: string | null;
  __injectStale: boolean;
  __staleInjected: boolean;
  __heldFrames: number;
  __releaseFrames(): void;
  __deliverFrame(key: string, frame: Frame): void;
}

async function installProbe(page: Page): Promise<void> {
  const fixture = [...await readFile(new URL("../../crates/pm-ghostty/testdata/native-state.snapshot", import.meta.url))];
  await page.addInitScript((bytes) => {
    const target = window as ProbeWindow;
    target.__geometrySamples = [];
    target.__holdTerminal = null;
    target.__injectStale = false;
    target.__staleInjected = false;
    target.__heldFrames = 0;
    let stage: Stage | undefined;
    let queue: Array<{ key: string; frame: Frame }> = [];
    Object.defineProperty(target, "__pmStage", {
      configurable: true,
      get: () => stage,
      set(value: Stage | undefined) {
        stage = value;
        if (!value) return;
        const receive = value.receiveFrame.bind(value);
        target.__deliverFrame = receive;
        value.receiveFrame = (key, frame) => {
          if (key !== target.__holdTerminal) return receive(key, frame);
          if (frame.replay && target.__injectStale && !target.__staleInjected) {
            target.__staleInjected = true;
            receive(key, { ...frame, data: new Uint8Array(bytes) });
          }
          queue.push({ key, frame });
          target.__heldFrames += 1;
        };
        target.__releaseFrames = () => {
          target.__holdTerminal = null;
          const pending = queue;
          queue = [];
          for (const entry of pending) receive(entry.key, entry.frame);
        };
      },
    });
    let hadScreen = false;
    const sample = () => {
      const shown: Sample["shown"] = [];
      for (const layer of stage?.layers.values() ?? []) {
        if (!layer.el.isConnected || getComputedStyle(layer.el).visibility !== "visible" || !layer.curtainEl.hidden) continue;
        const painted = layer.term.renderSnapshot();
        const measured = layer.term.cellSize;
        if (!painted || !measured) continue;
        const text = painted.rowData.map((row) => row.text).join("\n");
        if (text.trim()) shown.push({ key: layer.key, painted: { cols: painted.cols, rows: painted.rows }, measured, text });
      }
      hadScreen ||= shown.length > 0;
      if (stage?.visibleId && hadScreen) {
        const sessionRoute = location.hash.match(/^#\/session\/(\d+)(?:[/?]|$)/);
        const selected = sessionRoute ? `s:${sessionRoute[1]}` : stage.visibleId;
        target.__geometrySamples.push({ selected, shown });
        if (target.__geometrySamples.length > 2_000) target.__geometrySamples.shift();
      }
      requestAnimationFrame(sample);
    };
    requestAnimationFrame(sample);
  }, fixture);
}

function samples(page: Page): Promise<Sample[]> {
  return page.evaluate(() => (window as ProbeWindow).__geometrySamples);
}

async function clearSamples(page: Page): Promise<void> {
  await page.evaluate(() => { (window as ProbeWindow).__geometrySamples = []; });
}

async function twoPaints(page: Page): Promise<void> {
  await page.evaluate(() => new Promise<void>((resolve) => requestAnimationFrame(() => requestAnimationFrame(() => resolve()))));
}

function expectStable(samples: Sample[], expected?: Grid): void {
  expect(samples.length).toBeGreaterThan(0);
  expect(samples.filter((sample) => sample.shown.length > 1), "a switch never exposes two terminal layers").toEqual([]);
  expect(samples.flatMap((sample) => sample.shown.filter((entry) => entry.key !== sample.selected)),
    "only the selected terminal can be exposed").toEqual([]);
  const shown = samples.flatMap((sample) => sample.shown);
  expect(shown.filter((entry) => entry.painted.cols !== entry.measured.cols || entry.painted.rows !== entry.measured.rows),
    "a terminal is never exposed at a provisional size").toEqual([]);
  if (expected) expect(shown.filter((entry) => entry.painted.cols !== expected.cols || entry.painted.rows !== expected.rows)).toEqual([]);
}

test("cold switches hide the outgoing frame immediately while replay is delayed, including rapid returns", async ({ page, isolatedDaemon }) => {
  await installProbe(page);
  await logIn(page);
  const first = isolatedDaemon.session("browser-e2e")!.id;
  const second = isolatedDaemon.session("browser-e2e-two")!.id;
  await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title", { hasText: /^browser-e2e$/ }) }).click();
  await expectTerminalRevealed(page, `s:${first}`);
  await clearSamples(page);
  await page.evaluate((id) => { (window as ProbeWindow).__holdTerminal = `s:${id}`; }, second);
  const secondRow = page.locator(".sb-session").filter({ has: page.locator(".sb-session-title", { hasText: /^browser-e2e-two$/ }) });
  await secondRow.click();
  await expect.poll(() => page.evaluate(() => (window as ProbeWindow).__heldFrames)).toBeGreaterThan(0);
  await twoPaints(page);
  expect((await samples(page)).at(-1)?.shown.map((entry) => entry.key)).toEqual([]);
  expect(await page.evaluate((id) => {
    const layer = (window as ProbeWindow).__pmStage!.layers.get(`s:${id}`)!;
    return layer.el.contains(document.activeElement);
  }, second), "input focuses the selected terminal while its screen is covered").toBe(true);
  await page.keyboard.type("echo cold-switch-input\n");
  await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title", { hasText: /^browser-e2e$/ }) }).click();
  await secondRow.click();
  await twoPaints(page);
  expectStable(await samples(page));
  await page.evaluate(() => (window as ProbeWindow).__releaseFrames());
  await expectTerminalRevealed(page, `s:${second}`);
  await twoPaints(page);
  expectStable(await samples(page));
  await expect.poll(() => samples(page).then((entries) => entries.at(-1)?.shown[0]?.text.includes("OUT cold-switch-input"))).toBe(true);
  expect(await page.evaluate((id) => (window as ProbeWindow).__pmStage!.layers.get(`s:${id}`)!.term.renderSnapshot()!
    .rowData.some((row) => row.text.includes("cold-switch-input")), first)).toBe(false);
});

test("a stale-size first snapshot stays covered until the real-size frame paints", async ({ page, isolatedDaemon }) => {
  await installProbe(page);
  await logIn(page);
  const id = isolatedDaemon.session("browser-e2e")!.id;
  await page.evaluate((sessionId) => {
    const target = window as ProbeWindow;
    target.__pmStage?.disposeSession(BigInt(sessionId));
    target.__holdTerminal = `s:${sessionId}`;
    target.__injectStale = true;
  }, id);
  await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title", { hasText: /^browser-e2e$/ }) }).click();
  await expect.poll(() => page.evaluate(() => (window as ProbeWindow).__staleInjected)).toBe(true);
  await twoPaints(page);
  const covered = await page.evaluate((sessionId) => {
    const layer = (window as ProbeWindow).__pmStage!.layers.get(`s:${sessionId}`)!;
    return !layer.curtainEl.hidden || getComputedStyle(layer.el).visibility !== "visible";
  }, id);
  expect(covered, "the stale snapshot cannot be revealed just because decoding completed").toBe(true);
  await page.evaluate(() => (window as ProbeWindow).__releaseFrames());
  await expectTerminalRevealed(page, `s:${id}`);
  await twoPaints(page);
  expectStable(await samples(page));
});

test("a warm replacement snapshot cannot repaint the screen at a stale size", async ({ page, isolatedDaemon }) => {
  await installProbe(page);
  await logIn(page);
  const id = isolatedDaemon.session("browser-e2e")!.id;
  await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title", { hasText: /^browser-e2e$/ }) }).click();
  await expectTerminalRevealed(page, `s:${id}`);
  const fixture = [...await readFile(new URL("../../crates/pm-ghostty/testdata/native-state.snapshot", import.meta.url))];
  await clearSamples(page);
  const before = await page.evaluate(({ sessionId, bytes }) => {
    const target = window as ProbeWindow;
    const key = `s:${sessionId}`;
    const snapshot = target.__pmStage!.layers.get(key)!.term.renderSnapshot()!;
    target.__holdTerminal = key;
    target.__deliverFrame(key, { data: new Uint8Array(bytes), replay: true, snapshot: true, ghosttySnapshot: true });
    return { cols: snapshot.cols, rows: snapshot.rows, text: snapshot.rowData.map((row) => row.text).join("\n") };
  }, { sessionId: id, bytes: fixture });
  await twoPaints(page);
  expectStable(await samples(page), before);
  expect((await samples(page)).at(-1)?.shown[0]?.text).toBe(before.text);
  await expect.poll(() => page.evaluate(() => (window as ProbeWindow).__heldFrames)).toBeGreaterThan(0);
  await page.evaluate(() => (window as ProbeWindow).__releaseFrames());
  await twoPaints(page);
  expectStable(await samples(page), before);
});

test("a fixed viewport keeps one grid through warm, cold, and refreshed views", async ({ page, isolatedDaemon }, testInfo) => {
  await installProbe(page);
  await logIn(page);
  const ids = [isolatedDaemon.session("browser-e2e")!.id, isolatedDaemon.session("browser-e2e-two")!.id];
  for (const title of ["browser-e2e", "browser-e2e-two"]) {
    await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title", { hasText: new RegExp(`^${title}$`) }) }).click();
    await expectTerminalRevealed(page, "s:");
    await page.locator('.term-layer[style*="visibility: visible"] .ghostty-input').focus();
    await page.keyboard.type("gridtui\n");
    await expect.poll(() => samples(page).then((entries) => entries.at(-1)?.shown[0]?.text.includes("STATUS "))).toBe(true);
  }
  const expected = (await samples(page)).at(-1)!.shown[0].measured;
  await clearSamples(page);
  for (const id of [...ids, ...ids, ...ids]) {
    await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title", { hasText: id === ids[0] ? /^browser-e2e$/ : /^browser-e2e-two$/ }) }).click();
    await expectTerminalRevealed(page, `s:${id}`);
    await twoPaints(page);
  }
  await page.evaluate((id) => (window as ProbeWindow).__pmStage!.disposeSession(BigInt(id)), ids[0]);
  await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title", { hasText: /^browser-e2e$/ }) }).click();
  await expectTerminalRevealed(page, `s:${ids[0]}`);
  await twoPaints(page);
  const switched = await samples(page);
  expectStable(switched, expected);
  const refreshed: Sample[][] = [];
  for (const id of ids) {
    await page.goto(`${process.env.PM_E2E_BASE_URL}#/session/${id}`);
    await page.reload();
    await expectTerminalRevealed(page, `s:${id}`);
    await twoPaints(page);
    const observed = await samples(page);
    expectStable(observed, expected);
    refreshed.push(observed);
  }
  await testInfo.attach("fixed-geometry-frames", { body: JSON.stringify({ expected, switched, refreshed }), contentType: "application/json" });
});


test("a held replay never stretches the painted font when its container changes size", async ({ page, isolatedDaemon }) => {
  await installProbe(page);
  await logIn(page);
  const id = isolatedDaemon.session("browser-e2e")!.id;
  await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title", { hasText: /^browser-e2e$/ }) }).click();
  await expectTerminalRevealed(page, `s:${id}`);
  const fixture = [...await readFile(new URL("../../crates/pm-ghostty/testdata/native-state.snapshot", import.meta.url))];
  await page.evaluate(({ sessionId, bytes }) => {
    const target = window as ProbeWindow;
    const key = `s:${sessionId}`;
    target.__holdTerminal = key;
    target.__deliverFrame(key, { data: new Uint8Array(bytes), replay: true, snapshot: true, ghosttySnapshot: true });
  }, { sessionId: id, bytes: fixture });
  const canvas = page.locator('.term-layer[style*="visibility: visible"] .ghostty-canvas');
  for (const width of ["70%", "100%"]) {
    await page.evaluate((width) => {
      const stage = (window as ProbeWindow).__pmStage!;
      stage.layers.get(stage.visibleId!)!.el.style.width = width;
    }, width);
    await twoPaints(page);
    const scale = await canvas.evaluate((node) => {
      const bounds = node.getBoundingClientRect();
      const canvas = node as HTMLCanvasElement;
      return { x: bounds.width * devicePixelRatio / canvas.width, y: bounds.height * devicePixelRatio / canvas.height };
    });
    expect(scale.x, "a held image preserves horizontal glyph size").toBeCloseTo(1);
    expect(scale.y, "a held image preserves vertical glyph size").toBeCloseTo(1);
  }
  await page.evaluate(() => (window as ProbeWindow).__releaseFrames());
  await twoPaints(page);
});


test("directory-bar height changes cannot expose or resize the outgoing session", async ({ page, isolatedDaemon }, testInfo) => {
  isolatedDaemon.seedSessionForward("browser-e2e-two", "preview");
  await isolatedDaemon.restart();
  await installProbe(page);
  await logIn(page);
  const first = isolatedDaemon.session("browser-e2e")!.id;
  const second = isolatedDaemon.session("browser-e2e-two")!.id;
  const row = (title: string) => page.locator(".sb-session").filter({
    has: page.locator(".sb-session-title", { hasText: new RegExp(`^${title}$`) }),
  });
  await row("browser-e2e").click();
  await expectTerminalRevealed(page, `s:${first}`);
  await expect(page.locator(".forwards-bar")).toBeHidden();
  await twoPaints(page);
  const firstGrid = (await samples(page)).at(-1)!.shown[0].painted;
  await page.screenshot({ path: testInfo.outputPath("atomic-before.png") });
  await page.evaluate((id) => { (window as ProbeWindow).__holdTerminal = `s:${id}`; }, second);
  await clearSamples(page);
  await row("browser-e2e-two").click();
  await expect(page.locator(".forwards-bar")).toBeVisible();
  await expect.poll(() => page.evaluate(() => (window as ProbeWindow).__heldFrames)).toBeGreaterThan(0);
  await twoPaints(page);
  await page.screenshot({ path: testInfo.outputPath("atomic-loading.png") });
  const loading = (await samples(page)).filter((sample) => sample.selected === `s:${second}`);
  expect(loading.length).toBeGreaterThan(0);
  expect(loading.every((sample) => sample.shown.length === 0), "the outgoing session is hidden throughout cold attachment").toBe(true);
  expect(await page.evaluate((id) => {
    const snapshot = (window as ProbeWindow).__pmStage!.layers.get(`s:${id}`)!.term.renderSnapshot()!;
    return { cols: snapshot.cols, rows: snapshot.rows };
  }, first), "the outgoing buffer keeps its original geometry").toEqual(firstGrid);
  await page.evaluate(() => (window as ProbeWindow).__releaseFrames());
  await expectTerminalRevealed(page, `s:${second}`);
  await twoPaints(page);
  expectStable(await samples(page));
  const secondGrid = (await samples(page)).at(-1)!.shown[0].painted;
  expect(secondGrid.rows).toBeLessThan(firstGrid.rows);
  await page.screenshot({ path: testInfo.outputPath("atomic-ready.png") });
  for (const [title, id, grid] of [
    ["browser-e2e", first, firstGrid],
    ["browser-e2e-two", second, secondGrid],
    ["browser-e2e", first, firstGrid],
  ] as const) {
    await clearSamples(page);
    await row(title).click();
    await expectTerminalRevealed(page, `s:${id}`);
    await twoPaints(page);
    const observed = await samples(page);
    expectStable(observed);
    expectStable(observed.filter((sample) => sample.selected === `s:${id}`), grid);
  }
});


test("a preloaded session keeps its previous geometry after reload across different directory bars", async ({ page, isolatedDaemon }, testInfo) => {
  isolatedDaemon.seedSessionForward("browser-e2e-two", "preview");
  await isolatedDaemon.restart();
  await installProbe(page);
  await logIn(page);
  const first = isolatedDaemon.session("browser-e2e")!.id;
  const second = isolatedDaemon.session("browser-e2e-two")!.id;
  const row = (title: string) => page.locator(".sb-session").filter({
    has: page.locator(".sb-session-title", { hasText: new RegExp(`^${title}$`) }),
  });
  await row("browser-e2e-two").click();
  await expectTerminalRevealed(page, `s:${second}`);
  const historyLines = 1000;
  const input = page.locator('.term-layer[style*="visibility: visible"] .ghostty-input');
  await input.focus();
  await input.pressSequentially(`lineout ${historyLines}`);
  await input.press("Enter");
  await expect.poll(async () => (await samples(page)).at(-1)?.shown[0]?.text ?? "").toContain("HISTORY-0999");
  await twoPaints(page);
  const targetFrame = (await samples(page)).at(-1)!.shown[0];
  const targetGrid = targetFrame.painted;
  await page.screenshot({ path: testInfo.outputPath("reload-before.png") });
  await row("browser-e2e").click();
  await expectTerminalRevealed(page, `s:${first}`);
  await page.reload();
  await expectTerminalRevealed(page, `s:${first}`);
  await expect.poll(() => page.evaluate(() =>
    (window as unknown as { __pmStage: { warmStats(): { warmed: number } } }).__pmStage.warmStats().warmed,
  )).toBe(2);
  const attachments: Grid[] = [];
  page.on("websocket", (socket) => {
    const url = new URL(socket.url());
    if (!url.pathname.startsWith("/ws/terminal/")) return;
    attachments.push({ cols: Number(url.searchParams.get("cols")), rows: Number(url.searchParams.get("rows")) });
  });
  await clearSamples(page);
  const target = (await row("browser-e2e-two").boundingBox())!;
  await page.mouse.move(target.x + target.width / 2, target.y + target.height / 2);
  await page.mouse.down();
  await expect.poll(() => page.evaluate((id) =>
    (window as ProbeWindow).__pmStage!.layers.get(`s:${id}`)?.term.cellSize ?? null,
  second)).not.toBeNull();
  await twoPaints(page);
  await page.mouse.up();
  await expectTerminalRevealed(page, `s:${second}`);
  await expect.poll(() => page.evaluate((id) =>
    (window as ProbeWindow).__pmStage!.debugSnapshot().find((entry) => entry.key === `s:${id}`)?.socket?.replayCount ?? 0,
  second)).toBeGreaterThan(0);
  await twoPaints(page);
  expect(attachments).toEqual([targetGrid]);
  const observed = (await samples(page)).filter((sample) => sample.selected === `s:${second}`);
  expectStable(observed, targetGrid);
  expect(observed.flatMap((sample) => sample.shown).filter((entry) => entry.text !== targetFrame.text),
    "cached and live frames keep the same visible content in place").toEqual([]);
  await page.screenshot({ path: testInfo.outputPath("reload-after.png") });
  await testInfo.attach("reload-geometry", {
    body: JSON.stringify({ previous: targetGrid, attachments, shown: observed.flatMap((sample) => sample.shown.map((entry) => entry.painted)) }),
    contentType: "application/json",
  });
});
