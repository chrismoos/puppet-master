import { describe, expect, it, vi } from "vitest";
import type { TerminalClipboard } from "./terminalClipboard";
import {
  isCopyShortcut,
  registerForegroundCommandHandler,
  wireTerminalClipboard,
  type CopyTerminal,
} from "./terminalHostWiring";

function b64(text: string): string {
  return Buffer.from(text, "utf8").toString("base64");
}

class FakeTerminal implements CopyTerminal {
  selection = "";
  keyHandler: ((event: KeyboardEvent) => boolean) | null = null;
  selectionHandlers = new Set<() => void>();
  private oscHandlers = new Map<number, Array<(data: string) => boolean>>();
  parser = {
    registerOscHandler: (id: number, callback: (data: string) => boolean) => {
      this.oscHandlers.set(id, [...(this.oscHandlers.get(id) ?? []), callback]);
      return { dispose: () => this.oscHandlers.set(id, (this.oscHandlers.get(id) ?? []).filter((h) => h !== callback)) };
    },
  };

  getSelection(): string { return this.selection; }
  hasSelection(): boolean { return this.selection !== ""; }
  onSelectionComplete(listener: () => void) {
    this.selectionHandlers.add(listener);
    return { dispose: () => this.selectionHandlers.delete(listener) };
  }
  attachCustomKeyEventHandler(handler: (event: KeyboardEvent) => boolean): void { this.keyHandler = handler; }
  completeSelection(): void { for (const listener of this.selectionHandlers) listener(); }

  osc(id: number, data: string): boolean {
    return (this.oscHandlers.get(id) ?? []).some((handler) => handler(data));
  }

  registered(id: number): number {
    return this.oscHandlers.get(id)?.length ?? 0;
  }
}

function fakeClipboard() {
  return {
    copySelection: vi.fn(() => Promise.resolve()),
    copyFromApplication: vi.fn(() => Promise.resolve({ kind: "copied" as const, chars: 0 })),
  };
}

function key(overrides: Partial<KeyboardEvent>): KeyboardEvent {
  return { type: "keydown", metaKey: false, ctrlKey: false, shiftKey: false, key: "", ...overrides } as KeyboardEvent;
}

describe("registerForegroundCommandHandler", () => {
  it("claims only prefixed OSC 777 payloads and reports the command", () => {
    const term = new FakeTerminal();
    const onCommand = vi.fn();
    const registration = registerForegroundCommandHandler(term, onCommand);
    expect(term.osc(777, "pm-command;cargo build")).toBe(true);
    expect(onCommand).toHaveBeenCalledWith("cargo build");
    expect(term.osc(777, "notify;other")).toBe(false);
    registration.dispose();
    expect(term.registered(777)).toBe(0);
  });
});

describe("isCopyShortcut", () => {
  it("accepts cmd+c and ctrl+shift+c on keydown only", () => {
    expect(isCopyShortcut(key({ metaKey: true, key: "c" }))).toBe(true);
    expect(isCopyShortcut(key({ ctrlKey: true, shiftKey: true, key: "C" }))).toBe(true);
    expect(isCopyShortcut(key({ ctrlKey: true, key: "c" }))).toBe(false);
    expect(isCopyShortcut(key({ type: "keyup", metaKey: true, key: "c" }))).toBe(false);
  });
});

describe("wireTerminalClipboard", () => {
  it("routes OSC 52 writes to the application copy path and refuses reads", () => {
    const term = new FakeTerminal();
    const clipboard = fakeClipboard();
    wireTerminalClipboard(term, clipboard as unknown as TerminalClipboard);
    expect(term.osc(52, `c;${b64("from the agent")}`)).toBe(true);
    expect(clipboard.copyFromApplication).toHaveBeenCalledWith("from the agent");
    expect(term.osc(52, "c;?")).toBe(false);
    expect(clipboard.copyFromApplication).toHaveBeenCalledTimes(1);
  });

  it("copies the selection when the pointer gesture completes and on the copy shortcut", () => {
    const term = new FakeTerminal();
    const clipboard = fakeClipboard();
    wireTerminalClipboard(term, clipboard as unknown as TerminalClipboard);

    term.completeSelection();
    expect(clipboard.copySelection).not.toHaveBeenCalled();
    term.selection = "picked";
    term.completeSelection();
    expect(clipboard.copySelection).toHaveBeenCalledWith("picked");

    expect(term.keyHandler!(key({ metaKey: true, key: "c" }))).toBe(false);
    expect(clipboard.copySelection).toHaveBeenCalledTimes(2);
    expect(term.keyHandler!(key({ key: "a" }))).toBe(true);
    term.selection = "";
    expect(term.keyHandler!(key({ metaKey: true, key: "c" }))).toBe(true);
    expect(clipboard.copySelection).toHaveBeenCalledTimes(2);
  });

  it("strips the row padding a selection carries, on both selection paths", () => {
    const term = new FakeTerminal();
    const clipboard = fakeClipboard();
    wireTerminalClipboard(term, clipboard as unknown as TerminalClipboard);

    term.selection = `first${" ".repeat(90)}\n  second${" ".repeat(88)}\n\n`;
    term.completeSelection();
    expect(clipboard.copySelection).toHaveBeenCalledWith("first\n  second");

    term.keyHandler!(key({ metaKey: true, key: "c" }));
    expect(clipboard.copySelection).toHaveBeenLastCalledWith("first\n  second");
  });

  it("holds the clipboard when the selection is nothing but padding", () => {
    const term = new FakeTerminal();
    const clipboard = fakeClipboard();
    wireTerminalClipboard(term, clipboard as unknown as TerminalClipboard);

    term.selection = " ".repeat(120);
    term.completeSelection();
    expect(clipboard.copySelection).not.toHaveBeenCalled();
  });

  it("writes an OSC 52 copy exactly as the application named it", () => {
    const term = new FakeTerminal();
    const clipboard = fakeClipboard();
    wireTerminalClipboard(term, clipboard as unknown as TerminalClipboard);
    const exact = "token \n  trailing space kept   ";
    expect(term.osc(52, `c;${b64(exact)}`)).toBe(true);
    expect(clipboard.copyFromApplication).toHaveBeenCalledWith(exact);
  });

  it("releases the selection callback and OSC 52 registration on dispose", () => {
    const term = new FakeTerminal();
    const dispose = wireTerminalClipboard(term, fakeClipboard() as unknown as TerminalClipboard);
    expect(term.registered(52)).toBe(1);
    expect(term.selectionHandlers).toHaveLength(1);
    dispose();
    expect(term.registered(52)).toBe(0);
    expect(term.selectionHandlers).toHaveLength(0);
  });
});
