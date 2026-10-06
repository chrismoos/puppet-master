import { describe, expect, it } from "vitest";

import {
  bindClientToForeground,
  isForeground,
  watchForeground,
  type AppStateSource,
  type AppStateStatus,
} from "./lifecycle";

function fakeAppState(initial: AppStateStatus) {
  const listeners = new Set<(state: AppStateStatus) => void>();
  const source: AppStateSource = {
    currentState: initial,
    addEventListener: (_type, listener) => {
      listeners.add(listener);
      return { remove: () => listeners.delete(listener) };
    },
  };
  const emit = (state: AppStateStatus) => {
    source.currentState = state;
    for (const listener of listeners) listener(state);
  };
  return { source, emit, listeners };
}

describe("isForeground", () => {
  it("counts only the active state as foregrounded", () => {
    expect(isForeground("active")).toBe(true);
    for (const state of ["inactive", "background", "unknown", "extension"] as AppStateStatus[]) {
      expect(isForeground(state)).toBe(false);
    }
  });
});

describe("watchForeground", () => {
  it("fires one background edge across inactive and background states", () => {
    const { source, emit } = fakeAppState("active");
    const events: string[] = [];
    watchForeground(source, {
      onForeground: () => events.push("fg"),
      onBackground: () => events.push("bg"),
    });
    emit("inactive");
    emit("background");
    emit("active");
    expect(events).toEqual(["bg", "fg"]);
  });

  it("does not fire foreground when already foregrounded", () => {
    const { source, emit } = fakeAppState("active");
    const events: string[] = [];
    watchForeground(source, {
      onForeground: () => events.push("fg"),
      onBackground: () => events.push("bg"),
    });
    emit("active");
    expect(events).toEqual([]);
  });

  it("stops observing after unsubscribe", () => {
    const { source, emit, listeners } = fakeAppState("active");
    const events: string[] = [];
    const unsubscribe = watchForeground(source, {
      onForeground: () => events.push("fg"),
      onBackground: () => events.push("bg"),
    });
    unsubscribe();
    emit("background");
    expect(events).toEqual([]);
    expect(listeners.size).toBe(0);
  });
});

describe("bindClientToForeground", () => {
  function fakeClient() {
    const calls: string[] = [];
    return { calls, client: { start: () => calls.push("start"), stop: () => calls.push("stop") } };
  }

  it("starts immediately when foregrounded and follows app-state edges", () => {
    const { source, emit } = fakeAppState("active");
    const { calls, client } = fakeClient();
    bindClientToForeground(client, source);
    emit("background");
    emit("active");
    expect(calls).toEqual(["start", "stop", "start"]);
  });

  it("defers the first start until the app foregrounds", () => {
    const { source, emit } = fakeAppState("background");
    const { calls, client } = fakeClient();
    bindClientToForeground(client, source);
    expect(calls).toEqual([]);
    emit("active");
    expect(calls).toEqual(["start"]);
  });

  it("stops the client when unbound", () => {
    const { source } = fakeAppState("active");
    const { calls, client } = fakeClient();
    const unbind = bindClientToForeground(client, source);
    unbind();
    expect(calls).toEqual(["start", "stop"]);
  });
});
