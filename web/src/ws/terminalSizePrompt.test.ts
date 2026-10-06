// @vitest-environment jsdom
import { describe, expect, it, vi } from "vitest";
import { createTerminalSizePrompt } from "./terminalSizePrompt";

describe("terminal size banner", () => {
  it("keeps the terminal interactive and routes Update and Dismiss choices", () => {
    const host = document.createElement("div");
    const resize = vi.fn();
    const prompt = createTerminalSizePrompt(host, () => ({ cols: 120, rows: 40 }), resize);
    prompt.reset();
    prompt.requested({ cols: 120, rows: 40 });
    prompt.observe({ cols: 120, rows: 40, local: true });
    prompt.observe({ cols: 50, rows: 24, local: false });
    const banner = host.querySelector<HTMLDivElement>(".terminal-size-prompt")!;
    expect(banner.hidden).toBe(false);
    expect(banner.getAttribute("role")).toBe("status");
    const buttons = banner.querySelectorAll("button");
    buttons[1].click();
    expect(banner.hidden).toBe(true);
    expect(prompt.blocked()).toBe(true);
    expect(resize).not.toHaveBeenCalled();
    prompt.reset();
    prompt.requested({ cols: 120, rows: 40 });
    prompt.observe({ cols: 120, rows: 40, local: true });
    prompt.observe({ cols: 50, rows: 24, local: false });
    buttons[0].click();
    expect(resize).toHaveBeenCalledExactlyOnceWith({ cols: 120, rows: 40 });
    expect(banner.hidden).toBe(true);
    prompt.dispose();
    expect(host.querySelector(".terminal-size-prompt")).toBeNull();
  });
});
