import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import { createStatusGrace, STATUS_GRACE_MS } from "./statusGrace";

beforeEach(() => vi.useFakeTimers());
afterEach(() => vi.useRealTimers());

describe("createStatusGrace", () => {
  it("suppresses intermediate statuses during the grace window", () => {
    const values: string[] = [];
    const { setStatus } = createStatusGrace((s) => values.push(s));
    setStatus("authorizing");
    setStatus("connecting");
    expect(values).toEqual([]);
  });

  it("reveals the pending status after the grace period", () => {
    const values: string[] = [];
    const { setStatus } = createStatusGrace((s) => values.push(s));
    setStatus("authorizing");
    setStatus("connecting");
    vi.advanceTimersByTime(STATUS_GRACE_MS);
    expect(values).toEqual(["connecting"]);
  });

  it("shows online immediately and ends the grace", () => {
    const values: string[] = [];
    const { setStatus } = createStatusGrace((s) => values.push(s));
    setStatus("authorizing");
    setStatus("online");
    expect(values).toEqual(["online"]);
    // Further statuses show immediately (grace is over)
    setStatus("reconnecting");
    expect(values).toEqual(["online", "reconnecting"]);
  });

  it("shows rejected immediately and ends the grace", () => {
    const values: string[] = [];
    const { setStatus } = createStatusGrace((s) => values.push(s));
    setStatus("rejected");
    expect(values).toEqual(["rejected"]);
  });

  it("passes through all statuses after the grace expires", () => {
    const values: string[] = [];
    const { setStatus } = createStatusGrace((s) => values.push(s));
    setStatus("authorizing");
    vi.advanceTimersByTime(STATUS_GRACE_MS);
    setStatus("connecting");
    expect(values).toEqual(["authorizing", "connecting"]);
  });

  it("does not fire the timer after dispose", () => {
    const values: string[] = [];
    const ctrl = createStatusGrace((s) => values.push(s));
    ctrl.setStatus("authorizing");
    ctrl.dispose();
    vi.advanceTimersByTime(STATUS_GRACE_MS);
    expect(values).toEqual([]);
  });

  it("does not fire the timer if online arrives within the grace", () => {
    const values: string[] = [];
    const { setStatus } = createStatusGrace((s) => values.push(s));
    setStatus("authorizing");
    setStatus("connecting");
    setStatus("online");
    vi.advanceTimersByTime(STATUS_GRACE_MS);
    // Only "online" should have been emitted, not the pending "connecting"
    expect(values).toEqual(["online"]);
  });
});
