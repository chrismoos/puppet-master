import { describe, expect, it, vi } from "vitest";
import { TerminalSizePrompt } from "./terminalSizePrompt";

function harness() {
  let local = { cols: 120, rows: 40 };
  const changed = vi.fn();
  const resize = vi.fn();
  const prompt = new TerminalSizePrompt(() => local, changed, resize);
  prompt.reset();
  prompt.requested(local);
  prompt.observe({ ...local, local: true });
  changed.mockClear();
  return { prompt, changed, resize, setLocal: (size: typeof local) => { local = size; } };
}

describe("TerminalSizePrompt", () => {
  it("offers a choice immediately when another viewer owns a different size", () => {
    const h = harness();
    h.prompt.observe({ cols: 50, rows: 24, local: false });
    expect(h.changed).toHaveBeenLastCalledWith(true);
    expect(h.prompt.blocked()).toBe(true);
    expect(h.resize).not.toHaveBeenCalled();
    h.prompt.update();
    expect(h.resize).toHaveBeenCalledExactlyOnceWith({ cols: 120, rows: 40 });
    expect(h.prompt.blocked()).toBe(false);
  });

  it("never offers a choice for local ownership, regardless of echo dimensions", () => {
    const h = harness();
    for (let cols = 121; cols < 180; cols++) {
      h.setLocal({ cols, rows: 40 });
      h.prompt.localChanged();
      h.prompt.requested({ cols, rows: 40 });
      h.prompt.observe({ cols: cols - 1, rows: 30, local: true });
    }
    expect(h.changed).not.toHaveBeenCalledWith(true);
    expect(h.prompt.blocked()).toBe(false);
  });

  it("dismisses subsequent remote ownership until a new local claim", () => {
    const h = harness();
    h.prompt.observe({ cols: 50, rows: 24, local: false });
    h.prompt.dismiss();
    h.prompt.observe({ cols: 60, rows: 25, local: false });
    expect(h.changed).toHaveBeenLastCalledWith(false);
    expect(h.prompt.blocked()).toBe(true);
    h.setLocal({ cols: 130, rows: 45 });
    expect(h.prompt.localChanged()).toBe(true);
    expect(h.prompt.blocked()).toBe(false);
    h.prompt.requested({ cols: 130, rows: 45 });
    h.prompt.observe({ cols: 130, rows: 45, local: true });
    h.prompt.observe({ cols: 50, rows: 24, local: false });
    expect(h.changed).toHaveBeenLastCalledWith(true);
  });

  it("follows a remote owner at matching dimensions without showing a prompt", () => {
    const h = harness();
    h.prompt.observe({ cols: 120, rows: 40, local: false });
    expect(h.changed).toHaveBeenLastCalledWith(false);
    expect(h.prompt.blocked()).toBe(true);
  });

  it("keeps first-open and a local layout change quiet until a claim is acknowledged", () => {
    const h = harness();
    h.prompt.reset();
    h.prompt.observe({ cols: 50, rows: 24, local: false });
    expect(h.changed).not.toHaveBeenCalledWith(true);
    h.setLocal({ cols: 130, rows: 45 });
    h.prompt.localChanged();
    h.prompt.observe({ cols: 50, rows: 24, local: false });
    expect(h.changed).not.toHaveBeenCalledWith(true);
    h.prompt.requested({ cols: 130, rows: 45 });
    h.prompt.observe({ cols: 130, rows: 45, local: true });
    h.prompt.observe({ cols: 50, rows: 24, local: false });
    expect(h.changed).toHaveBeenLastCalledWith(true);
  });
  it("does not automatically claim again after opening has been acknowledged", () => {
    const h = harness();
    expect(h.prompt.needsClaim()).toBe(false);
    h.prompt.requested({ cols: 130, rows: 45 });
    expect(h.prompt.needsClaim()).toBe(false);
    h.prompt.observe({ cols: 130, rows: 45, local: true });
    expect(h.prompt.needsClaim()).toBe(false);
    h.prompt.observe({ cols: 50, rows: 24, local: false });
    expect(h.prompt.needsClaim()).toBe(false);
    h.setLocal({ cols: 130, rows: 45 });
    h.prompt.localChanged();
    expect(h.prompt.needsClaim()).toBe(true);
  });

});
