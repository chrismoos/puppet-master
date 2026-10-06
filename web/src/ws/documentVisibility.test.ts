import { describe, expect, it } from "vitest";
import { documentVisible, watchDocumentVisibility } from "./documentVisibility";

class FakeDocument {
  visibilityState = "visible";
  private listeners = new Set<() => void>();

  addEventListener(_type: "visibilitychange", listener: () => void): void {
    this.listeners.add(listener);
  }

  removeEventListener(_type: "visibilitychange", listener: () => void): void {
    this.listeners.delete(listener);
  }

  become(state: string): void {
    this.visibilityState = state;
    for (const listener of this.listeners) listener();
  }

  listenerCount(): number {
    return this.listeners.size;
  }
}

describe("watchDocumentVisibility", () => {
  it("treats every state except hidden as visible", () => {
    const doc = new FakeDocument();
    expect(documentVisible(doc)).toBe(true);
    doc.visibilityState = "prerender";
    expect(documentVisible(doc)).toBe(true);
    doc.visibilityState = "hidden";
    expect(documentVisible(doc)).toBe(false);
  });

  it("reports only real transitions and stops after unsubscribe", () => {
    const doc = new FakeDocument();
    const edges: boolean[] = [];
    const stop = watchDocumentVisibility(doc, (visible) => edges.push(visible));
    doc.become("visible");
    doc.become("hidden");
    doc.become("hidden");
    doc.become("visible");
    expect(edges).toEqual([false, true]);
    stop();
    doc.become("hidden");
    expect(edges).toEqual([false, true]);
    expect(doc.listenerCount()).toBe(0);
  });
});
