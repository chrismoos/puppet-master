import { describe, expect, it } from "vitest";
import type { AppStateSource, AppStateStatus } from "../adapters/lifecycle";
import type { HostMessage } from "./protocol";
import { bindTerminalVisibility } from "./visibility";

class FakeAppState implements AppStateSource {
  currentState: AppStateStatus = "active";
  private listeners = new Set<(state: AppStateStatus) => void>();

  addEventListener(_type: "change", listener: (state: AppStateStatus) => void) {
    this.listeners.add(listener);
    return { remove: () => this.listeners.delete(listener) };
  }

  change(state: AppStateStatus): void {
    this.currentState = state;
    for (const listener of this.listeners) listener(state);
  }

  listenerCount(): number {
    return this.listeners.size;
  }
}

describe("bindTerminalVisibility", () => {
  it("sends setVisible on foreground and background edges only", () => {
    const source = new FakeAppState();
    const sent: HostMessage[] = [];
    const stop = bindTerminalVisibility(source, (msg) => sent.push(msg));
    source.change("inactive");
    source.change("background");
    source.change("inactive");
    source.change("active");
    expect(sent).toEqual([
      { type: "setVisible", visible: false },
      { type: "setVisible", visible: true },
    ]);
    stop();
    source.change("background");
    expect(sent).toHaveLength(2);
    expect(source.listenerCount()).toBe(0);
  });
});
