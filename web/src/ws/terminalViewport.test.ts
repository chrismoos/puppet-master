import type { Terminal } from "@xterm/xterm";
import { beforeEach, describe, expect, it, vi } from "vitest";
import {
  captureTerminalViewport,
  loadTerminalViewport,
  restoreTerminalViewport,
  saveTerminalViewport,
} from "./terminalViewport";

function terminal(baseY: number, viewportY: number, type = "normal") {
  return {
    buffer: { active: { baseY, viewportY, type } },
    scrollToBottom: vi.fn(),
    scrollToLine: vi.fn(),
  } as unknown as Terminal;
}

describe("terminal viewport bookmarks", () => {
  const values = new Map<string, string>();
  beforeEach(() => {
    values.clear();
    vi.stubGlobal("sessionStorage", {
      getItem: (key: string) => values.get(key) ?? null,
      setItem: (key: string, value: string) => values.set(key, value),
      clear: () => values.clear(),
    });
  });

  it("restores scrolled-up positions by distance from the trimmed buffer tail", () => {
    const term = terminal(5_000, 4_880);
    const bookmark = captureTerminalViewport(term);
    (term.buffer.active as { baseY: number; viewportY: number }).baseY = 4_200;

    restoreTerminalViewport(term, bookmark);

    expect(term.scrollToLine).toHaveBeenCalledWith(4_080);
    expect(term.scrollToBottom).not.toHaveBeenCalled();
  });

  it("clamps a distance that exceeds the reconstructed scrollback", () => {
    const term = terminal(5_000, 100);
    const bookmark = captureTerminalViewport(term);
    (term.buffer.active as { baseY: number; viewportY: number }).baseY = 800;

    restoreTerminalViewport(term, bookmark);

    expect(term.scrollToLine).toHaveBeenCalledWith(0);
  });

  it("explicitly pins an at-bottom viewport after an async flush", () => {
    const term = terminal(5_000, 5_000);
    const bookmark = captureTerminalViewport(term);
    (term.buffer.active as { baseY: number; viewportY: number }).viewportY = 0;

    restoreTerminalViewport(term, bookmark);

    expect(term.scrollToBottom).toHaveBeenCalledOnce();
    expect(term.scrollToLine).not.toHaveBeenCalled();
  });

  it("does not apply a normal-buffer bookmark to the alternate buffer", () => {
    const term = terminal(100, 90, "normal");
    const bookmark = captureTerminalViewport(term);
    (term.buffer.active as { type: string }).type = "alternate";

    restoreTerminalViewport(term, bookmark);

    expect(term.scrollToBottom).not.toHaveBeenCalled();
    expect(term.scrollToLine).not.toHaveBeenCalled();
  });

  it("round trips reload bookmarks and ignores corrupt storage", () => {
    saveTerminalViewport("s:7", { bufferType: "normal", linesFromBottom: 42 });
    expect(loadTerminalViewport("s:7")).toEqual({ bufferType: "normal", linesFromBottom: 42 });
    sessionStorage.setItem("pm.terminalViewport.s:8", '{"linesFromBottom":-1}');
    expect(loadTerminalViewport("s:8")).toBeNull();
  });
});
