import type { TerminalStage } from "../src/ws/terminal";
import { expect, test, type Locator, type Page } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";

const CONTEXT_CHURN_COUNT = 12;
const TAB_VISIBILITY_CYCLES = 3;
const VISIBLE_TERMINAL = '.term-layer[style*="visible"]';

type ProbeWindow = Window & {
  __pmStage: Pick<TerminalStage, "debugSnapshot" | "disposeSession"> & {
    layers: Map<string, { el: HTMLElement }>;
  };
  __canvasProbe: { webglRequests: number; socketOpens: number; socketCloses: number; output: string };
};

async function openWithProbe(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const probe = { webglRequests: 0, socketOpens: 0, socketCloses: 0, output: "" };
    (window as unknown as ProbeWindow).__canvasProbe = probe;
    const getContext = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = new Proxy(getContext, {
      apply(target, receiver, args) {
        if (args[0] === "webgl" || args[0] === "webgl2" || args[0] === "experimental-webgl") {
          probe.webglRequests += 1;
        }
        return Reflect.apply(target, receiver, args);
      },
    });
    window.WebSocket = new Proxy(window.WebSocket, {
      construct(target, args) {
        const socket = Reflect.construct(target, args) as WebSocket;
        if (String(args[0]).includes("/ws/terminal/")) {
          probe.socketOpens += 1;
          socket.addEventListener("close", () => probe.socketCloses += 1);
          const decoder = new TextDecoder();
          const OUTPUT_TAG = 0x01;
          const OUTPUT_HEADER_BYTES = 10;
          socket.addEventListener("message", (event) => {
            if (!(event.data instanceof ArrayBuffer) || event.data.byteLength < OUTPUT_HEADER_BYTES) return;
            if (new DataView(event.data).getUint8(0) !== OUTPUT_TAG) return;
            probe.output += decoder.decode(new Uint8Array(event.data, OUTPUT_HEADER_BYTES), { stream: true });
          });
        }
        return socket;
      },
    });
  });
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await expect(page.locator(`${VISIBLE_TERMINAL} .ghostty-terminal`)).toBeVisible();
  await expectTerminalRevealed(page, "s:");
}

async function echo(page: Page, terminal: Locator, text: string): Promise<void> {
  await terminal.locator(".ghostty-input").focus();
  await page.keyboard.type(`echo ${text}`);
  await page.keyboard.press("Enter");
}

function probe(page: Page): Promise<ProbeWindow["__canvasProbe"]> {
  return page.evaluate(() => ({ ...(window as unknown as ProbeWindow).__canvasProbe }));
}

test("session terminal churn stays Canvas2D-only and releases replaced streams", async ({ page }) => {
  await openWithProbe(page);
  const sessions = page.locator(".sb-session");
  const keeperKey = await page.evaluate(() =>
    (window as unknown as ProbeWindow).__pmStage.debugSnapshot().find((layer) => layer.visible)!.key);

  for (let index = 0; index < CONTEXT_CHURN_COUNT; index += 1) {
    const target = index % 2;
    await sessions.nth(target).click();
    await expect(sessions.nth(target)).toHaveClass(/is-selected/);
    await expect(page.locator(`${VISIBLE_TERMINAL} .ghostty-terminal`)).toBeVisible();
    await sessions.nth(0).click();
    await expect(sessions.nth(0)).toHaveClass(/is-selected/);
    const leakedSurfaces = await page.evaluate((key) => {
      const stage = (window as unknown as ProbeWindow).__pmStage;
      const leaked: string[] = [];
      for (const layer of stage.debugSnapshot()) {
        if (layer.key !== key && layer.key.startsWith("s:")) {
          const host = stage.layers.get(layer.key)!.el;
          stage.disposeSession(BigInt(layer.key.slice(2)));
          if (host.querySelector(".ghostty-terminal")) leaked.push(layer.key);
        }
      }
      return leaked;
    }, keeperKey);
    expect(leakedSurfaces, "disposed terminal surfaces leave no wrapper behind").toEqual([]);
  }

  await echo(page, page.locator(VISIBLE_TERMINAL), "canvas-churn-live");
  await expect.poll(() => probe(page)).toMatchObject({ webglRequests: 0 });
  await expect.poll(() => probe(page).then((state) => state.output)).toContain("OUT canvas-churn-live");
  const state = await probe(page);
  expect(state.socketOpens).toBeGreaterThan(0);
  expect(state.socketCloses).toBeGreaterThan(0);
  await expect(page.locator(`${VISIBLE_TERMINAL} canvas`)).toHaveCount(1);
});

test("document visibility cycles repaint the selected Canvas2D terminal without reconnecting", async ({ page }) => {
  await openWithProbe(page);
  const terminal = page.locator(VISIBLE_TERMINAL);
  await echo(page, terminal, "canvas-before-visibility");
  await expect.poll(() => probe(page).then((state) => state.output)).toContain("OUT canvas-before-visibility");
  const before = await probe(page);

  for (let cycle = 0; cycle < TAB_VISIBILITY_CYCLES; cycle += 1) {
    for (const state of ["hidden", "visible"]) {
      await page.evaluate(async (visibilityState) => {
        Object.defineProperty(document, "visibilityState", { configurable: true, value: visibilityState });
        document.dispatchEvent(new Event("visibilitychange"));
        await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
      }, state);
    }
  }
  await page.evaluate(() => Reflect.deleteProperty(document, "visibilityState"));

  const after = await probe(page);
  expect(after.webglRequests).toBe(0);
  expect(after.socketOpens).toBe(before.socketOpens);
  expect(after.socketCloses).toBe(before.socketCloses);
  await echo(page, terminal, "canvas-after-visibility");
  await expect.poll(() => probe(page).then((state) => state.output)).toContain("OUT canvas-after-visibility");
});

test("workspace Canvas2D terminal keeps accepting input on its existing stream", async ({ page }) => {
  await openWithProbe(page);
  await page.getByTitle("new workspace").click();
  await page.getByRole("button", { name: "create workspace" }).click();
  await page.getByLabel("terminal shown in pane").selectOption({ label: "browser-e2e · agent" });
  const terminal = page.locator(".workspace-terminal-host");
  await expect(terminal.locator(".ghostty-terminal")).toBeVisible();
  await echo(page, terminal, "workspace-canvas-live");
  await expect.poll(() => probe(page).then((state) => state.output)).toContain("OUT workspace-canvas-live");

  const before = await probe(page);
  expect(before.webglRequests).toBe(0);
  expect(before.socketOpens).toBeGreaterThan(0);
  await expect(terminal.locator("canvas")).toHaveCount(1);
  await expect(page.locator(".workspace-renderer-warning")).toHaveCount(0);
  await echo(page, terminal, "workspace-canvas-still-live");
  await expect.poll(() => probe(page).then((state) => state.output)).toContain("OUT workspace-canvas-still-live");
  expect(await probe(page)).toMatchObject({ socketOpens: before.socketOpens, socketCloses: before.socketCloses });
});
