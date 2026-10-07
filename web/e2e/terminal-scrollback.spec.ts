import { expect, test, type Page } from "./fixtures";
import { installScrollbarClock, logIn, SCROLL_RENDER_MS } from "./support";
const LONG_OUTPUT_BYTES = 120_000;
const SHORT_OUTPUT_BYTES = 32_000;
const MINIMUM_SCROLLBAR_THUMB_PX = 20;

interface StreamMetric {
  bytes: number;
  created: number;
  closed: number;
}

type MetricWindow = Window & {
  __scrollbackStreams: Record<string, StreamMetric>;
};

async function logInWithProbe(page: Page): Promise<void> {
  await page.addInitScript(() => {
    localStorage.setItem("pm.viewTabs", JSON.stringify(["session:1", "workspace:99"]));
    const streams: Record<string, StreamMetric> = {};
    (window as MetricWindow).__scrollbackStreams = streams;
    const NativeWebSocket = window.WebSocket;
    window.WebSocket = new Proxy(NativeWebSocket, {
      construct(target, args) {
        const socket = Reflect.construct(target, args) as WebSocket;
        const url = String(args[0]);
        if (url.includes("/ws/terminal/")) {
          const metric = streams[url] ??= { bytes: 0, created: 0, closed: 0 };
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
  await logIn(page);
}

async function generateOutput(page: Page, bytes: number): Promise<void> {
  const before = await page.evaluate(() => Object.values((window as MetricWindow).__scrollbackStreams)
    .reduce((total, stream) => total + stream.bytes, 0));
  const textarea = page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
  await textarea.focus();
  await page.keyboard.type(`bigout ${bytes}`);
  await page.keyboard.press("Enter");
  await page.waitForFunction(({ previous, expected }) => Object.values((window as MetricWindow).__scrollbackStreams)
    .reduce((total, stream) => total + stream.bytes, 0) >= previous + expected, { previous: before, expected: bytes });
}

async function startOutput(page: Page, bytes: number): Promise<number> {
  const before = await page.evaluate(() => Object.values((window as MetricWindow).__scrollbackStreams)
    .reduce((total, stream) => total + stream.bytes, 0));
  const textarea = page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
  await textarea.focus();
  await page.keyboard.type(`pacedout ${bytes} 8000`);
  await page.keyboard.press("Enter");
  return before;
}

test("warm agent session switches expose each scrollback without a resize", async ({ page }) => {
  await installScrollbarClock(page);
  await logInWithProbe(page);
  const sessions = page.locator(".sb-session");
  const workspaceTabs = page.locator(".workspace-tab");
  const workspaceTabCount = await workspaceTabs.count();
  await expect(workspaceTabs.locator("small")).toHaveCount(workspaceTabCount);
  expect(await workspaceTabs.locator("small").allTextContents())
    .toEqual(Array(workspaceTabCount).fill("saved workspace"));
  await sessions.nth(0).click();
  await expect(workspaceTabs).toHaveCount(workspaceTabCount);
  await expect.poll(() => page.evaluate(() => localStorage.getItem("pm.viewTabs"))).toBeNull();
  await expect(sessions.nth(0)).toHaveClass(/is-selected/);
  await generateOutput(page, LONG_OUTPUT_BYTES);
  await sessions.nth(1).click();
  await expect(workspaceTabs).toHaveCount(workspaceTabCount);
  await expect(sessions.nth(1)).toHaveClass(/is-selected/);
  await expect.poll(() => page.evaluate(() => Object.keys((window as MetricWindow).__scrollbackStreams).length)).toBe(2);
  await generateOutput(page, SHORT_OUTPUT_BYTES);

  await sessions.nth(0).click();
  await expect(sessions.nth(0)).toHaveClass(/is-selected/);
  const longLayer = page.locator('.term-layer[style*="visible"]');
  const longSlider = longLayer.locator(".scrollbar.vertical .slider");
  const longScrollbar = longLayer.locator(".scrollbar.vertical");
  await expect.poll(() => longScrollbar.evaluate((element) => Number.parseFloat(getComputedStyle(element).opacity)))
    .toBe(0);
  const longScreen = await longLayer.locator(".xterm-screen").boundingBox();
  if (!longScreen) throw new Error("missing long terminal screen");
  await page.clock.pauseAt(new Date());
  await page.mouse.move(longScreen.x + longScreen.width / 2, longScreen.y + longScreen.height / 2);
  await page.mouse.wheel(0, -1_200);
  await page.clock.runFor(SCROLL_RENDER_MS);
  await expect.poll(() => longScrollbar.evaluate((element) => Number.parseFloat(getComputedStyle(element).opacity)))
    .toBe(1);
  const readingTop = await longSlider.evaluate((element) => Number.parseFloat((element as HTMLElement).style.top));
  const readingHeight = await longSlider.evaluate((element) => Number.parseFloat((element as HTMLElement).style.height));
  const socketsBeforeHiddenOutput = await page.evaluate(() => structuredClone((window as MetricWindow).__scrollbackStreams));
  await page.clock.resume();
  const hiddenOutputStart = await startOutput(page, 240_000);
  await sessions.nth(1).click();
  await expect(sessions.nth(1)).toHaveClass(/is-selected/);
  await page.waitForFunction(({ previous }) => {
    const streams = Object.values((window as MetricWindow).__scrollbackStreams);
    return streams.reduce((total, stream) => total + stream.bytes, 0) >= previous + 240_000
      || streams.some((stream) => stream.closed > 0);
  }, { previous: hiddenOutputStart });
  const hiddenStreamMetrics = await page.evaluate(() => structuredClone((window as MetricWindow).__scrollbackStreams));
  expect(hiddenStreamMetrics).toEqual(Object.fromEntries(Object.entries(hiddenStreamMetrics)
    .map(([url, stream]) => [url, { ...stream, closed: 0 }])));
  await sessions.nth(0).click();
  await expect(longSlider).toBeVisible();
  const returnedHeight = () => longSlider.evaluate((element) => Number.parseFloat((element as HTMLElement).style.height));
  if (readingHeight > MINIMUM_SCROLLBAR_THUMB_PX) {
    await expect.poll(returnedHeight).toBeLessThan(readingHeight);
  } else {
    await expect.poll(returnedHeight).toBeLessThanOrEqual(readingHeight);
  }
  const returnedTop = await longSlider.evaluate((element) => Number.parseFloat((element as HTMLElement).style.top));
  expect(returnedTop).toBeLessThan(readingTop + 50);
  const identityAfterHiddenOutput = await page.evaluate(() => Object.fromEntries(Object.entries(
    (window as MetricWindow).__scrollbackStreams,
  ).map(([url, stream]) => [url, { created: stream.created, closed: stream.closed }])));
  const identityBeforeHiddenOutput = Object.fromEntries(Object.entries(socketsBeforeHiddenOutput)
    .map(([url, stream]) => [url, { created: stream.created, closed: stream.closed }]));
  expect(identityAfterHiddenOutput).toEqual(identityBeforeHiddenOutput);

  const socketsBefore = await page.evaluate(() => structuredClone((window as MetricWindow).__scrollbackStreams));
  for (let index = 0; index < 12; index += 1) {
    const target = index % 2;
    await sessions.nth(target).click();
    await expect(sessions.nth(target)).toHaveClass(/is-selected/);
    const layer = page.locator('.term-layer[style*="visible"]');
    const slider = layer.locator(".scrollbar.vertical .slider");
    await expect(slider).toBeVisible();
    const height = await slider.evaluate((element) => Number.parseFloat((element as HTMLElement).style.height));
    expect(height).toBeGreaterThan(0);
    expect(height).toBeLessThan(900);
    const screen = await layer.locator(".xterm-screen").boundingBox();
    if (!screen) throw new Error("missing visible terminal screen");
    const topBefore = await slider.evaluate((element) => Number.parseFloat((element as HTMLElement).style.top));
    await page.mouse.move(screen.x + screen.width / 2, screen.y + screen.height / 2);
    await page.mouse.wheel(0, -600);
    await expect.poll(() => slider.evaluate((element) => Number.parseFloat((element as HTMLElement).style.top)))
      .toBeLessThan(topBefore);
    await page.mouse.wheel(0, 100_000);
  }

  const socketsAfter = await page.evaluate(() => structuredClone((window as MetricWindow).__scrollbackStreams));
  expect(socketsAfter).toEqual(socketsBefore);
  await expect.poll(() => longScrollbar.evaluate((element) => Number.parseFloat(getComputedStyle(element).opacity)))
    .toBe(0);
});
