import { describe, expect, it, vi } from "vitest";
import { ClipboardUnavailableError } from "@puppet-master/client-core/ws/clipboard";
import {
  TerminalClipboard,
  clipboardStatusText,
  type ClipboardButtonElement,
  type ClipboardDocument,
  type ClipboardStatusElement,
} from "./terminalClipboard";

class FakeElement implements ClipboardButtonElement {
  className = "";
  hidden = false;
  textContent: string | null = null;
  type = "";
  removed = false;
  children: FakeElement[] = [];
  private listeners = new Map<string, Array<(event: unknown) => void>>();

  appendChild(child: ClipboardStatusElement): void {
    this.children.push(child as FakeElement);
  }

  addEventListener(type: string, listener: (event: unknown) => void): void {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
  }

  remove(): void {
    this.removed = true;
  }

  fire(type: string): void {
    for (const listener of this.listeners.get(type) ?? []) listener({ type });
  }
}

class FakeDocument implements ClipboardDocument {
  private listeners = new Map<string, Array<(event: unknown) => void>>();

  createElement(tag: "div" | "span"): ClipboardStatusElement;
  createElement(tag: "button"): ClipboardButtonElement;
  createElement(): FakeElement {
    return new FakeElement();
  }

  addEventListener(type: string, listener: (event: unknown) => void): void {
    this.listeners.set(type, [...(this.listeners.get(type) ?? []), listener]);
  }

  removeEventListener(type: string, listener: (event: unknown) => void): void {
    this.listeners.set(type, (this.listeners.get(type) ?? []).filter((entry) => entry !== listener));
  }

  gesture(type: "pointerdown" | "keydown"): void {
    for (const listener of this.listeners.get(type) ?? []) listener({ type });
  }

  listenerCount(): number {
    return [...this.listeners.values()].reduce((sum, entries) => sum + entries.length, 0);
  }
}

function notAllowed(): Error {
  const error = new Error("Document is not focused.");
  error.name = "NotAllowedError";
  return error;
}

function harness(outcomes: Array<"ok" | "reject" | "unavailable">) {
  const doc = new FakeDocument();
  const writes: string[] = [];
  const write = vi.fn((text: string) => {
    writes.push(text);
    const outcome = outcomes.shift() ?? "ok";
    if (outcome === "ok") return Promise.resolve();
    if (outcome === "unavailable") return Promise.reject(new ClipboardUnavailableError());
    return Promise.reject(notAllowed());
  });
  const timeouts: Array<{ handler: () => void; ms: number }> = [];
  const timers = {
    setTimeout: (handler: () => void, ms: number) => {
      timeouts.push({ handler, ms });
      return timeouts.length;
    },
    clearTimeout: vi.fn(),
  };
  const clipboard = new TerminalClipboard(doc, write, timers);
  clipboard.attach();
  const el = clipboard.el as FakeElement;
  const [label, button] = el.children;
  return { clipboard, doc, writes, write, timeouts, timers, el, label, button };
}

async function settle(): Promise<void> {
  await new Promise((resolve) => setTimeout(resolve, 0));
}

describe("clipboardStatusText", () => {
  it("describes each state for the status element", () => {
    expect(clipboardStatusText({ kind: "idle" })).toBe("");
    expect(clipboardStatusText({ kind: "copied", chars: 14 })).toBe("Copied 14 chars");
    expect(clipboardStatusText({ kind: "pending", chars: 14, insecureOrigin: false })).toBe("Copy of 14 chars is waiting for a click or keypress");
    expect(clipboardStatusText({ kind: "pending", chars: 14, insecureOrigin: true })).toContain("not HTTPS or localhost");
    expect(clipboardStatusText({ kind: "failed", chars: 1, reason: "nope" })).toBe("Copy failed: nope");
  });
});

describe("TerminalClipboard", () => {
  it("starts hidden with a hidden completion button", () => {
    const { el, button } = harness([]);
    expect(el.hidden).toBe(true);
    expect(el.className).toBe("terminal-clipboard-status");
    expect(button.hidden).toBe(true);
    expect(button.type).toBe("button");
  });

  it("shows a transient copied notice when the application copy resolves", async () => {
    const { clipboard, el, label, button, timeouts } = harness(["ok"]);
    await clipboard.copyFromApplication("sent 14 chars!");
    expect(el.hidden).toBe(false);
    expect(el.className).toBe("terminal-clipboard-status is-copied");
    expect(label.textContent).toBe("Copied 14 chars");
    expect(button.hidden).toBe(true);
    expect(timeouts).toHaveLength(1);
    timeouts[0].handler();
    expect(el.hidden).toBe(true);
    expect(clipboard.current()).toEqual({ kind: "idle" });
  });

  it("parks a refused application copy and completes it on a document gesture", async () => {
    const { clipboard, doc, el, label, button, writes, timeouts } = harness(["reject", "ok"]);
    await clipboard.copyFromApplication("later");
    expect(el.hidden).toBe(false);
    expect(el.className).toBe("terminal-clipboard-status is-pending");
    expect(label.textContent).toContain("waiting");
    expect(button.hidden).toBe(false);
    expect(timeouts).toHaveLength(0);

    doc.gesture("pointerdown");
    expect(writes).toEqual(["later", "later"]);
    await settle();
    expect(clipboard.current()).toEqual({ kind: "copied", chars: 5 });
    expect(button.hidden).toBe(true);
  });

  it("completes a parked copy from its own button and from a keydown", async () => {
    const { clipboard, doc, button, writes } = harness(["reject", "ok", "reject", "ok"]);
    await clipboard.copyFromApplication("via button");
    button.fire("click");
    await settle();
    expect(clipboard.current()).toEqual({ kind: "copied", chars: 10 });

    await clipboard.copyFromApplication("via key");
    doc.gesture("keydown");
    await settle();
    expect(clipboard.current()).toEqual({ kind: "copied", chars: 7 });
    expect(writes).toEqual(["via button", "via button", "via key", "via key"]);
  });

  it("writes nothing on a gesture when no copy is pending", () => {
    const { doc, write } = harness([]);
    doc.gesture("pointerdown");
    doc.gesture("keydown");
    expect(write).not.toHaveBeenCalled();
  });

  it("reports a failed gesture retry and hides it later", async () => {
    const { clipboard, doc, el, label, timeouts } = harness(["reject", "reject"]);
    await clipboard.copyFromApplication("stuck");
    doc.gesture("pointerdown");
    await settle();
    expect(el.className).toBe("terminal-clipboard-status is-failed");
    expect(label.textContent).toBe("Copy failed: Document is not focused.");
    expect(timeouts).toHaveLength(1);
    expect(timeouts[0].ms).toBeGreaterThan(0);
    timeouts[0].handler();
    expect(el.hidden).toBe(true);
    expect(clipboard.current()).toEqual({ kind: "idle" });
  });

  it("stays silent on a successful selection copy but parks a refused one", async () => {
    const { clipboard, el, label, doc, writes } = harness(["ok", "reject", "ok"]);
    await clipboard.copySelection("picked");
    expect(el.hidden).toBe(true);

    await clipboard.copySelection("picked");
    expect(el.hidden).toBe(false);
    expect(el.className).toBe("terminal-clipboard-status is-pending");
    expect(label.textContent).toBe("Copy of 6 chars is waiting for a click or keypress");
    doc.gesture("keydown");
    await settle();
    expect(writes).toEqual(["picked", "picked", "picked"]);
    expect(clipboard.current()).toEqual({ kind: "copied", chars: 6 });
  });

  it("names the insecure origin when navigator.clipboard is undefined and completes on a gesture", async () => {
    const { clipboard, el, label, button, doc } = harness(["unavailable", "ok"]);
    await clipboard.copyFromApplication("over http");
    expect(el.className).toBe("terminal-clipboard-status is-pending");
    expect(label.textContent).toBe(
      "Copy of 9 chars is waiting for a click or keypress (this page is not HTTPS or localhost, so copies need a click or keypress)",
    );
    expect(button.hidden).toBe(false);
    doc.gesture("pointerdown");
    await settle();
    expect(clipboard.current()).toEqual({ kind: "copied", chars: 9 });
  });

  it("reports a copy that fails again without a clipboard API", async () => {
    const { clipboard, el, label, doc } = harness(["unavailable", "unavailable"]);
    await clipboard.copyFromApplication("x");
    doc.gesture("pointerdown");
    await settle();
    expect(el.className).toBe("terminal-clipboard-status is-failed");
    expect(label.textContent).toBe("Copy failed: clipboard unavailable on this origin");
  });

  it("cancels a scheduled hide when a newer copy arrives", async () => {
    const { clipboard, timers, timeouts } = harness(["ok", "reject"]);
    await clipboard.copyFromApplication("one");
    expect(timeouts).toHaveLength(1);
    await clipboard.copyFromApplication("two");
    expect(timers.clearTimeout).toHaveBeenCalledWith(1);
    expect(clipboard.current()).toEqual({ kind: "pending", chars: 3, insecureOrigin: false });
  });

  it("attaches gesture listeners once and removes them on detach and dispose", () => {
    const { clipboard, doc, el } = harness([]);
    expect(doc.listenerCount()).toBe(2);
    clipboard.attach();
    expect(doc.listenerCount()).toBe(2);
    clipboard.detach();
    expect(doc.listenerCount()).toBe(0);
    clipboard.attach();
    clipboard.dispose();
    expect(doc.listenerCount()).toBe(0);
    expect(el.removed).toBe(true);
  });
});
