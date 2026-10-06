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
  private oscHandlers = new Map<number, Array<(data: string) => boolean>>();
  parser = {
    registerOscHandler: (id: number, callback: (data: string) => boolean) => {
      this.oscHandlers.set(id, [...(this.oscHandlers.get(id) ?? []), callback]);
      return { dispose: () => this.oscHandlers.set(id, (this.oscHandlers.get(id) ?? []).filter((h) => h !== callback)) };
    },
  };

  getSelection(): string { return this.selection; }
  hasSelection(): boolean { return this.selection !== ""; }
  attachCustomKeyEventHandler(handler: (event: KeyboardEvent) => boolean): void { this.keyHandler = handler; }

  osc(id: number, data: string): boolean {
    return (this.oscHandlers.get(id) ?? []).some((handler) => handler(data));
  }

  registered(id: number): number {
    return this.oscHandlers.get(id)?.length ?? 0;
  }
}

class FakeHost {
  listeners: Array<() => void> = [];
  addEventListener(_type: "mouseup", listener: () => void): void { this.listeners.push(listener); }
  removeEventListener(_type: "mouseup", listener: () => void): void {
    this.listeners = this.listeners.filter((entry) => entry !== listener);
  }
  mouseUp(): void { for (const listener of this.listeners) listener(); }
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
    wireTerminalClipboard(term, new FakeHost(), clipboard as unknown as TerminalClipboard);
    expect(term.osc(52, `c;${b64("from the agent")}`)).toBe(true);
    expect(clipboard.copyFromApplication).toHaveBeenCalledWith("from the agent");
    expect(term.osc(52, "c;?")).toBe(false);
    expect(clipboard.copyFromApplication).toHaveBeenCalledTimes(1);
  });

  it("copies the selection on mouseup and on the copy shortcut", () => {
    const term = new FakeTerminal();
    const host = new FakeHost();
    const clipboard = fakeClipboard();
    wireTerminalClipboard(term, host, clipboard as unknown as TerminalClipboard);

    host.mouseUp();
    expect(clipboard.copySelection).not.toHaveBeenCalled();
    term.selection = "picked";
    host.mouseUp();
    expect(clipboard.copySelection).toHaveBeenCalledWith("picked");

    expect(term.keyHandler!(key({ metaKey: true, key: "c" }))).toBe(false);
    expect(clipboard.copySelection).toHaveBeenCalledTimes(2);
    expect(term.keyHandler!(key({ key: "a" }))).toBe(true);
    term.selection = "";
    expect(term.keyHandler!(key({ metaKey: true, key: "c" }))).toBe(true);
    expect(clipboard.copySelection).toHaveBeenCalledTimes(2);
  });

  it("releases the host listener and the OSC 52 registration on dispose", () => {
    const term = new FakeTerminal();
    const host = new FakeHost();
    const dispose = wireTerminalClipboard(term, host, fakeClipboard() as unknown as TerminalClipboard);
    expect(term.registered(52)).toBe(1);
    expect(host.listeners).toHaveLength(1);
    dispose();
    expect(term.registered(52)).toBe(0);
    expect(host.listeners).toHaveLength(0);
  });
});
