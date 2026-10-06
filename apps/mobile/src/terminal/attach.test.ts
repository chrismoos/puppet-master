import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { TerminalAttachController, type AttachHooks, type AttachPhase } from "./attach";

interface Harness {
  controller: TerminalAttachController;
  phases: AttachPhase[];
  inits: Array<{ ticket: string; generation: string }>;
  mints: string[];
  resolveMint(ticket: string): void;
  rejectMint(reason: string): void;
}

function harness(generation = "3"): Harness {
  const phases: AttachPhase[] = [];
  const inits: Array<{ ticket: string; generation: string }> = [];
  const mints: string[] = [];
  const pending: Array<{ resolve(ticket: string): void; reject(err: Error): void }> = [];
  const hooks: AttachHooks = {
    mintTicket: (gen) => {
      mints.push(gen);
      return new Promise((resolve, reject) => pending.push({ resolve, reject }));
    },
    sendInit: (ticket, gen) => inits.push({ ticket, generation: gen }),
    onPhase: (phase) => phases.push(phase),
  };
  return {
    controller: new TerminalAttachController(hooks, generation),
    phases,
    inits,
    mints,
    resolveMint: (ticket) => pending.shift()?.resolve(ticket),
    rejectMint: (reason) => pending.shift()?.reject(new Error(reason)),
  };
}

async function settle(): Promise<void> {
  await Promise.resolve();
  await Promise.resolve();
}

beforeEach(() => {
  vi.useFakeTimers();
});

afterEach(() => {
  vi.useRealTimers();
});

describe("TerminalAttachController", () => {
  it("mints and sends init once the WebView is ready", async () => {
    const h = harness("3");
    h.controller.viewReady();
    expect(h.phases).toEqual([{ kind: "minting" }]);
    h.resolveMint("tick-1");
    await settle();
    expect(h.mints).toEqual(["3"]);
    expect(h.inits).toEqual([{ ticket: "tick-1", generation: "3" }]);
    expect(h.phases.at(-1)).toEqual({ kind: "initSent", generation: "3" });
  });

  it("surfaces a mint failure as a visible error with the reason", async () => {
    const h = harness();
    h.controller.viewReady();
    h.rejectMint("terminal generation changed");
    await settle();
    expect(h.phases.at(-1)).toEqual({ kind: "error", reason: "terminal generation changed" });
    expect(h.inits).toEqual([]);
  });

  it("manual retry mints again after a failure", async () => {
    const h = harness();
    h.controller.viewReady();
    h.rejectMint("offline");
    await settle();
    h.controller.retry();
    h.resolveMint("tick-2");
    await settle();
    expect(h.inits).toEqual([{ ticket: "tick-2", generation: "3" }]);
  });

  it("re-mints with backoff while the WebView keeps reconnecting", async () => {
    const h = harness();
    h.controller.viewReady();
    h.resolveMint("tick-1");
    await settle();

    h.controller.viewStatus("reconnecting");
    expect(h.mints).toHaveLength(1);
    await vi.advanceTimersByTimeAsync(500);
    expect(h.mints).toHaveLength(2);
    h.resolveMint("tick-2");
    await settle();
    expect(h.inits.at(-1)).toEqual({ ticket: "tick-2", generation: "3" });

    // The second consecutive failure waits twice as long.
    h.controller.viewStatus("reconnecting");
    await vi.advanceTimersByTimeAsync(500);
    expect(h.mints).toHaveLength(2);
    await vi.advanceTimersByTimeAsync(500);
    expect(h.mints).toHaveLength(3);
  });

  it("resets the backoff once the terminal comes online", async () => {
    const h = harness();
    h.controller.viewReady();
    h.resolveMint("tick-1");
    await settle();
    h.controller.viewStatus("reconnecting");
    await vi.advanceTimersByTimeAsync(500);
    h.resolveMint("tick-2");
    await settle();
    h.controller.viewStatus("online");
    h.controller.viewStatus("reconnecting");
    await vi.advanceTimersByTimeAsync(500);
    expect(h.mints).toHaveLength(3);
  });

  it("coalesces repeated reconnecting reports into one pending mint", async () => {
    const h = harness();
    h.controller.viewReady();
    h.resolveMint("tick-1");
    await settle();
    h.controller.viewStatus("reconnecting");
    h.controller.viewStatus("reconnecting");
    h.controller.authRejected();
    await vi.advanceTimersByTimeAsync(8_000);
    expect(h.mints).toHaveLength(2);
  });

  it("mints a fresh ticket immediately when the generation changes", async () => {
    const h = harness("3");
    h.controller.viewReady();
    h.resolveMint("tick-1");
    await settle();
    h.controller.setGeneration("4");
    expect(h.mints).toEqual(["3", "4"]);
    h.resolveMint("tick-2");
    await settle();
    expect(h.inits.at(-1)).toEqual({ ticket: "tick-2", generation: "4" });
  });

  it("discards a ticket minted for a generation that changed mid-mint", async () => {
    const h = harness("3");
    h.controller.viewReady();
    h.controller.setGeneration("4");
    h.resolveMint("stale-ticket");
    await settle();
    expect(h.inits).toEqual([]);
    expect(h.mints).toEqual(["3", "4"]);
    h.resolveMint("tick-2");
    await settle();
    expect(h.inits).toEqual([{ ticket: "tick-2", generation: "4" }]);
  });

  it("retries with the new generation when a mint fails after a mid-flight generation change", async () => {
    const h = harness("0");
    h.controller.viewReady();
    // The mint for gen=0 is in flight; meanwhile the real generation arrives.
    h.controller.setGeneration("1");
    // setGeneration tried mintAndInit but it was a no-op (minting=true).
    expect(h.mints).toEqual(["0"]);
    // The gen=0 mint fails (stale generation on the server).
    h.rejectMint("terminal generation changed");
    await settle();
    // The controller must retry with gen=1 instead of showing an error.
    expect(h.phases.every((p) => p.kind !== "error")).toBe(true);
    expect(h.mints).toEqual(["0", "1"]);
    h.resolveMint("good-ticket");
    await settle();
    expect(h.inits).toEqual([{ ticket: "good-ticket", generation: "1" }]);
  });

  it("ignores status and generation changes before the WebView is ready", () => {
    const h = harness("3");
    h.controller.viewStatus("reconnecting");
    h.controller.setGeneration("4");
    vi.advanceTimersByTime(8_000);
    expect(h.mints).toEqual([]);
  });

  it("does nothing after dispose", async () => {
    const h = harness();
    h.controller.viewReady();
    h.controller.dispose();
    h.resolveMint("tick-1");
    await settle();
    expect(h.inits).toEqual([]);
    h.controller.viewStatus("reconnecting");
    await vi.advanceTimersByTimeAsync(8_000);
    expect(h.mints).toHaveLength(1);
  });
});
