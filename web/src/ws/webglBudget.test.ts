// @vitest-environment jsdom
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import type { Terminal } from "@xterm/xterm";
import { WebglAddon } from "@xterm/addon-webgl";
import { attachWebglRenderer, MAX_WEBGL_TERMINALS, WebglBudget } from "./webglBudget";

vi.mock("@xterm/addon-webgl", () => ({ WebglAddon: vi.fn() }));
const handles: Array<{ dispose(): void }> = [];

function fakeAddon() {
  const canvas = document.createElement("canvas");
  const overlay = document.createElement("canvas");
  overlay.className = "xterm-link-layer";
  let lost = false;
  let screen: HTMLElement;
  let notifyLoss = () => {};
  const loseContext = vi.fn(() => { lost = true; });
  const gl = { isContextLost: () => lost, getExtension: vi.fn(() => ({ loseContext })) };
  const nativeLoss = vi.fn();
  const addon = {
    canvas, gl, loseContext, nativeLoss,
    activate: (host: HTMLElement, missingContext: boolean) => {
      screen = host.querySelector(".xterm-screen")!;
      screen.replaceChildren(overlay, canvas);
      Object.defineProperty(overlay, "getContext", { value: vi.fn(() => null) });
      Object.defineProperty(canvas, "getContext", { value: vi.fn(() => missingContext ? null : gl) });
      canvas.addEventListener("webglcontextlost", nativeLoss);
    },
    onContextLoss: vi.fn((callback: () => void) => {
      notifyLoss = callback;
      return { dispose: vi.fn() };
    }),
    dispose: vi.fn(() => {
      canvas.removeEventListener("webglcontextlost", nativeLoss);
      screen?.replaceChildren(Object.assign(document.createElement("div"), { className: "xterm-rows" }));
    }),
    notifyLoss: () => notifyLoss(),
    lose: () => {
      lost = true;
      canvas.dispatchEvent(new Event("webglcontextlost", { cancelable: true }));
    },
  };
  return addon;
}

beforeEach(() => {
  vi.useFakeTimers();
  vi.mocked(WebglAddon).mockImplementation(function () {
    return fakeAddon() as unknown as WebglAddon;
  });
});

afterEach(() => {
  for (const handle of handles.splice(0)) handle.dispose();
  vi.useRealTimers();
  document.body.replaceChildren();
});

function rendererFixture(options = { failLoad: false, missingContext: false }, visible = true) {
  const element = document.createElement("div");
  element.innerHTML = '<div class="xterm-screen"></div>';
  document.body.appendChild(element);
  const addons: ReturnType<typeof fakeAddon>[] = [];
  const terminal = {
    element,
    rows: 24,
    refresh: vi.fn(),
    loadAddon: vi.fn((addon: ReturnType<typeof fakeAddon>) => {
      addons.push(addon);
      addon.activate(element, options.missingContext);
      if (options.failLoad) throw new Error("activation failed");
    }),
  } as unknown as Terminal;
  const onIssue = vi.fn();
  const handle = attachWebglRenderer(terminal, onIssue, visible);
  handles.push(handle);
  return { handle, addons, terminal, onIssue, options, screen: element.querySelector<HTMLElement>(".xterm-screen")! };
}

describe("WebGL terminal budget", () => {
  it("refuses excess contexts and reuses a released slot only once", () => {
    const budget = new WebglBudget(2);
    const first = budget.acquire();
    const second = budget.acquire();
    expect(first).not.toBeNull();
    expect(second).not.toBeNull();
    expect(budget.acquire()).toBeNull();
    first?.();
    first?.();
    expect(budget.acquire()).not.toBeNull();
    expect(budget.acquire()).toBeNull();
    second?.();
  });
});

describe("GPU-only terminal lifecycle", () => {
  it("releases the GPU context despite a 2D overlay appearing first", () => {
    const fixture = rendererFixture();
    fixture.handle.dispose();
    fixture.handle.dispose();
    expect(fixture.addons[0].dispose).toHaveBeenCalledOnce();
    expect(fixture.addons[0].loseContext).toHaveBeenCalledOnce();
    expect(fixture.addons[0].gl.getExtension).toHaveBeenCalledWith("WEBGL_lose_context");
    expect(fixture.onIssue).not.toHaveBeenCalled();
  });

  it("recreates WebGL during the loss event instead of displaying HTML", () => {
    const fixture = rendererFixture();
    const original = fixture.addons[0];
    original.lose();
    expect(original.dispose).toHaveBeenCalledOnce();
    expect(original.nativeLoss).not.toHaveBeenCalled();
    expect(original.loseContext).not.toHaveBeenCalled();
    expect(fixture.addons).toHaveLength(2);
    expect(fixture.handle.active).toBe(true);
    expect(fixture.screen.querySelector(".xterm-rows")).toBeNull();
    expect(fixture.screen.style.visibility).toBe("");
    expect(fixture.onIssue.mock.calls).toEqual([["context-loss"], [null]]);
    expect(fixture.terminal.refresh).toHaveBeenLastCalledWith(0, fixture.terminal.rows - 1);
    original.notifyLoss();
    expect(fixture.addons).toHaveLength(2);
  });

  it("releases hidden renderers and reacquires WebGL when shown", () => {
    const fixture = rendererFixture(undefined, false);
    expect(fixture.addons).toHaveLength(0);
    fixture.handle.setVisible(true);
    expect(fixture.handle.active).toBe(true);
    fixture.handle.setVisible(false);
    expect(fixture.addons[0].loseContext).toHaveBeenCalledOnce();
    expect(fixture.handle.active).toBe(false);
    expect(fixture.screen.style.visibility).toBe("hidden");
    fixture.handle.setVisible(true);
    expect(fixture.addons).toHaveLength(2);
    expect(fixture.handle.active).toBe(true);
  });

  it("reuses a retained hidden renderer without creating another context", () => {
    const fixture = rendererFixture();
    fixture.handle.setVisible(false, true);
    fixture.handle.setVisible(true);
    expect(fixture.addons).toHaveLength(1);
    expect(fixture.addons[0].loseContext).not.toHaveBeenCalled();
    expect(fixture.handle.active).toBe(true);
  });

  it("reclaims a cached hidden context before making a visible terminal wait", () => {
    const active = Array.from({ length: MAX_WEBGL_TERMINALS }, () => rendererFixture());
    active[0].handle.setVisible(false, true);
    const incoming = rendererFixture();
    expect(active[0].handle.active).toBe(false);
    expect(active[0].addons[0].loseContext).toHaveBeenCalledOnce();
    expect(incoming.handle.active).toBe(true);
    expect(incoming.onIssue).not.toHaveBeenCalledWith("budget");
  });

  it("waits for a GPU lease without displaying HTML and resumes when one is released", async () => {
    const active = Array.from({ length: MAX_WEBGL_TERMINALS }, () => rendererFixture());
    const waiting = rendererFixture();
    expect(waiting.addons).toHaveLength(0);
    expect(waiting.handle.active).toBe(false);
    expect(waiting.onIssue).toHaveBeenCalledWith("budget");
    expect(waiting.screen.style.visibility).toBe("hidden");
    active[0].handle.setVisible(false);
    await vi.runAllTicks();
    expect(waiting.handle.active).toBe(true);
    expect(waiting.screen.querySelector(".xterm-rows")).toBeNull();
    expect(waiting.onIssue).toHaveBeenLastCalledWith(null);
  });

  it("hides failed activation and retries GPU rendering", async () => {
    const fixture = rendererFixture({ failLoad: true, missingContext: false });
    expect(fixture.handle.active).toBe(false);
    expect(fixture.addons[0].loseContext).toHaveBeenCalledOnce();
    expect(fixture.screen.style.visibility).toBe("hidden");
    expect(fixture.onIssue).toHaveBeenCalledWith("unavailable");
    fixture.options.failLoad = false;
    await vi.advanceTimersToNextTimerAsync();
    expect(fixture.handle.active).toBe(true);
    expect(fixture.screen.querySelector(".xterm-rows")).toBeNull();
    expect(fixture.onIssue).toHaveBeenLastCalledWith(null);
  });

  it("backs off repeated losses without painting the temporary HTML renderer", async () => {
    const fixture = rendererFixture();
    fixture.addons[0].lose();
    fixture.addons[1].lose();
    expect(fixture.addons).toHaveLength(2);
    expect(fixture.handle.active).toBe(false);
    expect(fixture.screen.style.visibility).toBe("hidden");
    expect(fixture.onIssue).toHaveBeenLastCalledWith("context-loss");
    await vi.advanceTimersToNextTimerAsync();
    expect(fixture.addons).toHaveLength(3);
    expect(fixture.handle.active).toBe(true);
    expect(fixture.screen.querySelector(".xterm-rows")).toBeNull();
  });

  it("cancels recovery on disposal and ignores late notifications", async () => {
    const fixture = rendererFixture();
    fixture.addons[0].lose();
    fixture.addons[1].lose();
    fixture.handle.dispose();
    const calls = fixture.onIssue.mock.calls.length;
    fixture.addons[1].notifyLoss();
    await vi.runAllTimersAsync();
    expect(fixture.addons).toHaveLength(2);
    expect(fixture.onIssue).toHaveBeenCalledTimes(calls);
    expect(fixture.handle.active).toBe(false);
  });

  it("never displays HTML when WebGL is unavailable", async () => {
    const fixture = rendererFixture({ failLoad: false, missingContext: true });
    await vi.advanceTimersToNextTimerAsync();
    expect(fixture.handle.active).toBe(false);
    expect(fixture.screen.style.visibility).toBe("hidden");
    expect(fixture.onIssue).toHaveBeenLastCalledWith("unavailable");
    expect(fixture.terminal.element!.querySelector<HTMLElement>(".terminal-gpu-status")!.hidden).toBe(false);
  });
});
