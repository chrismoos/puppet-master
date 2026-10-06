import { describe, expect, it, vi } from "vitest";
import {
  CLIPBOARD_UNAVAILABLE_MESSAGE,
  ClipboardUnavailableError,
  DeferredClipboardCopy,
  clipboardAvailable,
  clipboardFailureReason,
  writeClipboardText,
  type ClipboardCopyState,
} from "./clipboard";

function notAllowed(): Error {
  const error = new Error("Document is not focused.");
  error.name = "NotAllowedError";
  return error;
}

describe("writeClipboardText", () => {
  it("rejects with ClipboardUnavailableError instead of throwing when navigator.clipboard is undefined", async () => {
    expect(clipboardAvailable({})).toBe(false);
    expect(clipboardAvailable(undefined)).toBe(false);
    await expect(writeClipboardText("x", {})).rejects.toBeInstanceOf(ClipboardUnavailableError);
    await expect(writeClipboardText("x", undefined)).rejects.toThrow(CLIPBOARD_UNAVAILABLE_MESSAGE);
  });

  it("delegates to the host clipboard", async () => {
    const writeText = vi.fn(() => Promise.resolve());
    expect(clipboardAvailable({ clipboard: { writeText } })).toBe(true);
    await writeClipboardText("hello", { clipboard: { writeText } });
    expect(writeText).toHaveBeenCalledWith("hello");
  });

  it("turns a synchronous throw into a rejection", async () => {
    const host = { clipboard: { writeText: () => { throw new Error("boom"); } } };
    await expect(writeClipboardText("x", host)).rejects.toThrow("boom");
  });
});

describe("clipboardFailureReason", () => {
  it("prefers the error message and falls back to a generic reason", () => {
    expect(clipboardFailureReason(notAllowed())).toBe("Document is not focused.");
    expect(clipboardFailureReason("nope")).toBe("nope");
    expect(clipboardFailureReason(undefined)).toBe("copy failed");
  });
});

describe("DeferredClipboardCopy", () => {
  function harness(outcomes: Array<"ok" | "reject" | "unavailable">) {
    const writes: string[] = [];
    const states: ClipboardCopyState[] = [];
    const write = vi.fn((text: string) => {
      writes.push(text);
      const outcome = outcomes.shift() ?? "ok";
      if (outcome === "ok") return Promise.resolve();
      if (outcome === "unavailable") return Promise.reject(new ClipboardUnavailableError());
      return Promise.reject(notAllowed());
    });
    const copy = new DeferredClipboardCopy(write, (state) => states.push(state));
    return { copy, writes, states, write };
  }

  it("resolves immediately when the browser accepts the write", async () => {
    const { copy, writes, states } = harness(["ok"]);
    await expect(copy.copy("sent text")).resolves.toEqual({ kind: "copied", chars: 9 });
    expect(writes).toEqual(["sent text"]);
    expect(copy.hasPending()).toBe(false);
    expect(states).toEqual([{ kind: "copied", chars: 9 }]);
  });

  it("stays idle after a silent success such as a selection copy", async () => {
    const { copy, states } = harness(["ok"]);
    await expect(copy.copy("picked", { silentSuccess: true })).resolves.toEqual({ kind: "idle" });
    expect(states).toEqual([{ kind: "idle" }]);
  });

  it("parks a refused write and completes it on the next gesture", async () => {
    const { copy, writes, states } = harness(["reject", "ok"]);
    await expect(copy.copy("later")).resolves.toEqual({ kind: "pending", chars: 5, insecureOrigin: false });
    expect(copy.hasPending()).toBe(true);

    const completion = copy.completeFromGesture();
    expect(completion).not.toBeNull();
    expect(copy.hasPending()).toBe(false);
    await expect(completion).resolves.toEqual({ kind: "copied", chars: 5 });
    expect(writes).toEqual(["later", "later"]);
    expect(states).toEqual([
      { kind: "pending", chars: 5, insecureOrigin: false },
      { kind: "copied", chars: 5 },
    ]);
  });

  it("parks a write with no clipboard API as an insecure-origin copy and completes it on a gesture", async () => {
    const { copy, writes } = harness(["unavailable", "ok"]);
    await expect(copy.copy("http only")).resolves.toEqual({ kind: "pending", chars: 9, insecureOrigin: true });
    expect(copy.hasPending()).toBe(true);
    await expect(copy.completeFromGesture()).resolves.toEqual({ kind: "copied", chars: 9 });
    expect(writes).toEqual(["http only", "http only"]);
  });

  it("reports a gesture retry that still fails instead of parking it again", async () => {
    const { copy } = harness(["unavailable", "unavailable"]);
    await copy.copy("twice");
    await expect(copy.completeFromGesture()).resolves.toEqual({
      kind: "failed",
      chars: 5,
      reason: CLIPBOARD_UNAVAILABLE_MESSAGE,
    });
    expect(copy.hasPending()).toBe(false);
    expect(copy.completeFromGesture()).toBeNull();
  });

  it("returns null from a gesture when nothing is pending", () => {
    const { copy, write } = harness([]);
    expect(copy.completeFromGesture()).toBeNull();
    expect(write).not.toHaveBeenCalled();
  });

  it("keeps only the latest refused payload", async () => {
    const { copy, writes } = harness(["reject", "reject", "ok"]);
    await copy.copy("first");
    await copy.copy("second");
    await expect(copy.completeFromGesture()).resolves.toEqual({ kind: "copied", chars: 6 });
    expect(writes).toEqual(["first", "second", "second"]);
  });

  it("ignores the outcome of a write superseded by a newer copy", async () => {
    let resolveFirst: () => void = () => {};
    const write = vi.fn((text: string) => (text === "slow"
      ? new Promise<void>((resolve) => { resolveFirst = resolve; })
      : Promise.reject(notAllowed())));
    const states: ClipboardCopyState[] = [];
    const copy = new DeferredClipboardCopy(write, (state) => states.push(state));
    const slow = copy.copy("slow");
    await copy.copy("fast");
    resolveFirst();
    await slow;
    expect(copy.current()).toEqual({ kind: "pending", chars: 4, insecureOrigin: false });
    expect(states).toEqual([{ kind: "pending", chars: 4, insecureOrigin: false }]);
  });

  it("parks a write that never settles once the settle timeout elapses", async () => {
    const timeouts: Array<{ handler: () => void; ms: number }> = [];
    const write = vi.fn((_text: string) => (write.mock.calls.length === 1
      ? new Promise<void>(() => {})
      : Promise.resolve()));
    const copy = new DeferredClipboardCopy(write, () => {}, {
      settleTimeoutMs: 250,
      setTimeout: (handler, ms) => timeouts.push({ handler, ms }),
    });
    const attempt = copy.copy("hung");
    expect(timeouts).toEqual([{ handler: expect.any(Function), ms: 250 }]);
    timeouts[0].handler();
    await expect(attempt).resolves.toEqual({ kind: "pending", chars: 4, insecureOrigin: false });
    await expect(copy.completeFromGesture()).resolves.toEqual({ kind: "copied", chars: 4 });
  });

  it("can be dismissed back to idle", async () => {
    const { copy, states } = harness(["reject"]);
    await copy.copy("gone");
    copy.dismiss();
    expect(copy.hasPending()).toBe(false);
    expect(copy.current()).toEqual({ kind: "idle" });
    expect(states.at(-1)).toEqual({ kind: "idle" });
  });
});
