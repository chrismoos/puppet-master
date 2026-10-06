import { expect, test, type Locator, type Page } from "./fixtures";
import { logIn } from "./support";

interface SocketMetric {
  bytes: number;
  created: number;
  closed: number;
}

interface TerminalMetrics {
  rootId: string;
  layerId: string;
  rootConnected: boolean;
  layerConnected: boolean;
  rootParentClass: string;
  visibility: string;
  pointerEvents: string;
  screenWidth: number;
  screenHeight: number;
  canvasCount: number;
  viewportScrollTop: number;
  viewportScrollHeight: number;
  viewportClientHeight: number;
  cellHeight: number;
  rows: number;
  scrollbarHeight: number;
  baseY: number;
  viewportY: number;
  sliderTop: number;
  sliderHeight: number;
  scrollbarOpacity: string;
  textareaFocused: boolean;
  sockets: Record<string, SocketMetric>;
}

type ProbeWindow = Window & {
  __boardReturnProbe: {
    nextNodeId: number;
    roots: HTMLElement[];
    layers: HTMLElement[];
    sockets: Record<string, SocketMetric>;
  };
};

async function logInWithProbe(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const probe: ProbeWindow["__boardReturnProbe"] = {
      nextNodeId: 1,
      roots: [],
      layers: [],
      sockets: {},
    };
    (window as ProbeWindow).__boardReturnProbe = probe;

    const remember = (element: Element) => {
      if (!(element instanceof HTMLElement)) return;
      if (!element.matches(".term-stage, .term-layer")) return;
      if (!element.dataset.probeId) element.dataset.probeId = String(probe.nextNodeId++);
      const collection = element.matches(".term-stage") ? probe.roots : probe.layers;
      if (!collection.includes(element)) collection.push(element);
    };
    new MutationObserver((records) => {
      for (const record of records) {
        for (const node of record.addedNodes) {
          if (!(node instanceof Element)) continue;
          remember(node);
          for (const element of node.querySelectorAll(".term-stage, .term-layer")) remember(element);
        }
      }
    }).observe(document, { childList: true, subtree: true });

    const NativeWebSocket = window.WebSocket;
    window.WebSocket = new Proxy(NativeWebSocket, {
      construct(target, args) {
        const socket = Reflect.construct(target, args) as WebSocket;
        const url = String(args[0]);
        if (url.includes("/ws/terminal/")) {
          const metric = probe.sockets[url] ??= { bytes: 0, created: 0, closed: 0 };
          metric.created += 1;
          socket.addEventListener("close", () => metric.closed += 1);
          socket.addEventListener("message", (event) => {
            if (!(event.data instanceof ArrayBuffer) || event.data.byteLength < 10) return;
            if (new DataView(event.data).getUint8(0) !== 1) return;
            metric.bytes += event.data.byteLength - 10;
          });
        }
        return socket;
      },
    }) as typeof WebSocket;
  });

  await logIn(page, { minimumSessions: 2 });
}

async function generateHistory(page: Page): Promise<void> {
  const before = await page.evaluate(() => Object.values((window as ProbeWindow).__boardReturnProbe.sockets)
    .reduce((total, socket) => total + socket.bytes, 0));
  const textarea = page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
  await textarea.focus();
  await page.keyboard.type("lineout 800");
  await page.keyboard.press("Enter");
  await page.waitForFunction(({ previous }) => Object.values(
    (window as ProbeWindow).__boardReturnProbe.sockets,
  ).reduce((total, socket) => total + socket.bytes, 0) >= previous + 20_000, {
    previous: before,
  });
}

async function startBoardBurst(page: Page): Promise<number> {
  const before = await page.evaluate(() => Object.values((window as ProbeWindow).__boardReturnProbe.sockets)
    .reduce((total, socket) => total + socket.bytes, 0));
  await page.locator('.term-layer[style*="visible"] .xterm-helper-textarea').focus();
  await page.keyboard.type("lineout 800");
  await page.keyboard.press("Enter");
  return before;
}

async function waitForBoardBurst(page: Page, before: number): Promise<void> {
  await page.waitForFunction(({ previous }) => Object.values(
    (window as ProbeWindow).__boardReturnProbe.sockets,
  ).reduce((total, socket) => total + socket.bytes, 0) >= previous + 20_000, { previous: before });
}

function socketIdentity(metrics: TerminalMetrics): Record<string, { created: number; closed: number }> {
  return Object.fromEntries(Object.entries(metrics.sockets)
    .map(([url, socket]) => [url, { created: socket.created, closed: socket.closed }]));
}

async function terminalMetrics(page: Page): Promise<TerminalMetrics> {
  return page.evaluate(() => {
    const probe = (window as ProbeWindow).__boardReturnProbe;
    const root = probe.roots.at(-1);
    const layer = probe.layers.findLast((candidate) => candidate.style.visibility === "visible")
      ?? probe.layers.at(-1);
    if (!root || !layer) throw new Error("terminal probe did not retain a stage and layer");
    const viewport = layer.querySelector<HTMLElement>(".xterm-viewport");
    const screen = layer.querySelector<HTMLElement>(".xterm-screen");
    const textarea = layer.querySelector<HTMLElement>(".xterm-helper-textarea");
    const cell = layer.querySelector<HTMLElement>(".xterm-helper-textarea");
    const slider = layer.querySelector<HTMLElement>(".scrollbar.vertical .slider");
    const scrollbar = layer.querySelector<HTMLElement>(".scrollbar.vertical");
    if (!viewport || !screen || !textarea || !cell || !slider || !scrollbar) {
      throw new Error("terminal probe is missing xterm viewport elements");
    }
    const cellHeight = Number.parseFloat(cell.style.height);
    const rows = Math.round(screen.clientHeight / cellHeight);
    const sliderTop = Number.parseFloat(slider.style.top);
    const sliderHeight = Number.parseFloat(slider.style.height);
    const scrollbarHeight = scrollbar.clientHeight;
    // The slider-derived estimate saturates at the slider's minimum height,
    // so read the true buffer coordinates through the stage when available.
    interface StageWindow {
      __pmStage?: {
        layers: Map<string, {
          el: HTMLElement;
          term: { buffer: { active: { baseY: number; viewportY: number } } };
        }>;
      };
    }
    let baseY = Math.max(0, Math.round(rows * scrollbarHeight / sliderHeight - rows));
    let viewportY = baseY === 0
      ? 0
      : Math.round(sliderTop / (scrollbarHeight - sliderHeight) * baseY);
    const stage = (window as unknown as StageWindow).__pmStage;
    for (const candidate of stage?.layers.values() ?? []) {
      if (candidate.el !== layer) continue;
      baseY = candidate.term.buffer.active.baseY;
      viewportY = candidate.term.buffer.active.viewportY;
    }
    return {
      rootId: root.dataset.probeId!,
      layerId: layer.dataset.probeId!,
      rootConnected: root.isConnected,
      layerConnected: layer.isConnected,
      rootParentClass: root.parentElement?.className ?? "",
      visibility: layer.style.visibility,
      pointerEvents: layer.style.pointerEvents,
      screenWidth: screen.clientWidth,
      screenHeight: screen.clientHeight,
      canvasCount: layer.querySelectorAll("canvas").length,
      viewportScrollTop: viewport.scrollTop,
      viewportScrollHeight: viewport.scrollHeight,
      viewportClientHeight: viewport.clientHeight,
      cellHeight,
      rows,
      scrollbarHeight,
      baseY,
      viewportY,
      sliderTop,
      sliderHeight,
      scrollbarOpacity: getComputedStyle(scrollbar).opacity,
      textareaFocused: document.activeElement === textarea,
      sockets: structuredClone(probe.sockets),
    };
  });
}

async function wheelUp(page: Page, layer: Locator): Promise<number> {
  const slider = layer.locator(".scrollbar.vertical .slider");
  const screen = await layer.locator(".xterm-screen").boundingBox();
  if (!screen) throw new Error("visible terminal screen has no bounds");
  const before = await slider.evaluate((element) => Number.parseFloat((element as HTMLElement).style.top));
  await page.mouse.move(screen.x + screen.width / 2, screen.y + screen.height / 2);
  await page.mouse.wheel(0, -1_200);
  // e2e-real-time-wait: xterm's smooth scrollbar applies wheel movement on animation frames
  await page.waitForTimeout(100);
  return before - await slider.evaluate((element) => Number.parseFloat((element as HTMLElement).style.top));
}

async function dragSliderUp(page: Page, layer: Locator): Promise<number> {
  const slider = layer.locator(".scrollbar.vertical .slider");
  // The scrollbar overlays the terminal and only accepts input while
  // revealed by scrolling, so nudge the wheel before grabbing it.
  const screenBox = await layer.locator(".xterm-screen").boundingBox();
  if (screenBox) {
    await page.mouse.move(screenBox.x + screenBox.width / 2, screenBox.y + screenBox.height / 2);
    await page.mouse.wheel(0, -40);
    // e2e-real-time-wait: xterm's smooth scrollbar applies wheel movement on animation frames
    await page.waitForTimeout(120);
  }
  const bounds = await slider.boundingBox();
  if (!bounds) throw new Error("terminal scrollbar slider has no bounds");
  const before = await slider.evaluate((element) => Number.parseFloat((element as HTMLElement).style.top));
  await page.mouse.move(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2);
  await page.mouse.down();
  await page.mouse.move(bounds.x + bounds.width / 2, bounds.y + bounds.height / 2 - 80, { steps: 4 });
  await page.mouse.up();
  // e2e-real-time-wait: xterm's smooth scrollbar applies pointer drag on animation frames
  await page.waitForTimeout(100);
  return before - await slider.evaluate((element) => Number.parseFloat((element as HTMLElement).style.top));
}

test("board and item-detail return preserves real xterm scrollback without browser resize", async ({ page }) => {
  await logInWithProbe(page);
  const seededSession = () => page.locator(".sb-session-title")
    .filter({ hasText: /^browser-e2e$/ })
    .locator("xpath=ancestor::button[contains(@class, 'sb-session')][1]");
  const sessionRow = seededSession();
  await expect(sessionRow).toHaveCount(1);
  await sessionRow.click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);
  const sessionId = page.url().match(/#\/session\/(\d+)$/)?.[1];
  if (!sessionId) throw new Error("seeded browser session did not open");
  await generateHistory(page);
  await page.reload();
  await expect(page).toHaveURL(new RegExp(`#\\/session\\/${sessionId}$`));
  await expect(page.locator('.term-layer[style*="visible"]')).toBeVisible();
  await expect.poll(() => terminalMetrics(page).then((metric) => metric.baseY)).toBeGreaterThan(700);

  const visibleLayer = page.locator('.term-layer[style*="visible"]');
  const before = await terminalMetrics(page);
  expect(before.baseY).toBeGreaterThan(100);
  expect(Math.abs(before.viewportY - before.baseY)).toBeLessThanOrEqual(3);
  for (let index = 0; index < 4; index += 1) await page.keyboard.press("Shift+PageUp");
  const reading = await terminalMetrics(page);
  expect(reading.viewportY).toBeLessThan(reading.baseY - 20);
  expect(await dragSliderUp(page, visibleLayer)).toBeGreaterThan(20);
  for (let index = 0; index < 20; index += 1) await page.keyboard.press("Shift+PageDown");
  const departing = await terminalMetrics(page);
  expect(Math.abs(departing.viewportY - departing.baseY)).toBeLessThanOrEqual(3);

  const boardBurstStart = await startBoardBurst(page);
  await page.locator(".sb-board-link").click();
  await expect(page).toHaveURL(/#\/bucket\/1\/board/);
  await waitForBoardBurst(page, boardBurstStart);
  await expect(page.locator(".workbench-row").first()).toBeVisible();
  await page.locator(".workbench-row").first().click();
  const itemLink = page.locator('.inspector-titlebar a[href^="#/bucket/1/item/"]');
  const itemId = (await itemLink.getAttribute("href"))?.match(/\/item\/(\d+)/)?.[1];
  if (!itemId) throw new Error("board inspector did not expose its item route");
  await itemLink.click();
  await expect(page).toHaveURL(new RegExp(`#\\/bucket\\/1\\/item\\/${itemId}$`));

  const retainedBackground = page.locator(".restorable-view.is-board-background");
  await expect(retainedBackground).toHaveCount(1);
  await expect(retainedBackground).toHaveCSS("visibility", "hidden");
  const detached = await terminalMetrics(page);
  expect(detached.rootConnected).toBe(true);
  expect(detached.layerConnected).toBe(true);
  expect(detached.rootId).toBe(before.rootId);
  expect(detached.layerId).toBe(before.layerId);

  await seededSession().click();
  await expect(page).toHaveURL(new RegExp(`#\\/session\\/${sessionId}$`));
  await expect(visibleLayer).toBeVisible();
  // e2e-real-time-wait: capture the immediate post-route renderer state across animation frames
  await page.waitForTimeout(250);

  const returned = await terminalMetrics(page);
  const immediateDragDelta = await dragSliderUp(page, visibleLayer);
  for (let index = 0; index < 20; index += 1) await page.keyboard.press("Shift+PageDown");
  const immediateWheelDelta = await wheelUp(page, visibleLayer);
  await page.setViewportSize({ width: 1441, height: 1000 });
  await expect.poll(() => terminalMetrics(page).then((metric) => metric.baseY)).toBeGreaterThan(100);
  const resized = await terminalMetrics(page);
  const resizedDragDelta = await dragSliderUp(page, visibleLayer);
  for (let index = 0; index < 20; index += 1) await page.keyboard.press("Shift+PageDown");
  const resizedWheelDelta = await wheelUp(page, visibleLayer);

  console.log(`BOARD_RETURN_METRICS ${JSON.stringify({
    before,
    reading,
    departing,
    detached,
    returned,
    immediateDragDelta,
    immediateWheelDelta,
    resized,
    resizedDragDelta,
    resizedWheelDelta,
  })}`);

  expect(returned.rootId).toBe(before.rootId);
  expect(returned.layerId).toBe(before.layerId);
  expect(socketIdentity(returned)).toEqual(socketIdentity(before));
  expect(socketIdentity(resized)).toEqual(socketIdentity(before));
  expect(resized.baseY).toBeGreaterThan(100);
  expect(resizedDragDelta).toBeGreaterThan(20);

  // This is intentionally the final pre-fix gate: resize recovery is proven
  // before an inaccessible immediate return fails the untouched product.
  expect(returned.baseY).toBeGreaterThan(100);
  expect(returned.baseY).toBeGreaterThan(departing.baseY + 500);
  // A retained Board terminal remains live while hidden. Its viewport may preserve
  // a reading offset as output arrives, but returning must expose valid, nearby
  // scrollback before any resize repairs the geometry.
  expect(returned.viewportY).toBeLessThanOrEqual(returned.baseY);
  expect(returned.baseY - returned.viewportY).toBeLessThan(returned.rows);
  expect(immediateDragDelta).toBeGreaterThan(20);
  expect(immediateWheelDelta).toBeGreaterThan(0);
});
