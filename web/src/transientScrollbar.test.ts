import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import {
  SCROLLBAR_IDLE_MS,
  overNativeVerticalScrollbar,
  wireTransientScrollbar,
} from "./transientScrollbar";

type Listener = (event: never) => void;
type PointerPredicate = (event: PointerEvent) => boolean;

class FakeTarget {
  readonly classes = new Set<string>();
  private readonly listeners = new Map<string, Set<Listener>>();

  readonly classList = {
    add: (...names: string[]) => { for (const name of names) this.classes.add(name); },
    remove: (...names: string[]) => { for (const name of names) this.classes.delete(name); },
    contains: (name: string) => this.classes.has(name),
  };

  addEventListener = (type: string, listener: Listener): void => {
    const set = this.listeners.get(type) ?? new Set<Listener>();
    set.add(listener);
    this.listeners.set(type, set);
  };

  removeEventListener = (type: string, listener: Listener): void => {
    this.listeners.get(type)?.delete(listener);
  };

  dispatch(type: string, event: unknown = {}): void {
    for (const listener of [...(this.listeners.get(type) ?? [])]) listener(event as never);
  }

  listenerCount(): number {
    let total = 0;
    for (const set of this.listeners.values()) total += set.size;
    return total;
  }
}

const ACTIVE = "is-active-bar";

function harness(options: { onScrollbar?: PointerPredicate; revealOnHover?: boolean } = {}) {
  const host = new FakeTarget();
  const win = new FakeTarget();
  vi.stubGlobal("window", {
    setTimeout: (fn: () => void, ms: number) => setTimeout(fn, ms),
    clearTimeout: (id: number) => clearTimeout(id),
    addEventListener: win.addEventListener,
    removeEventListener: win.removeEventListener,
  });
  const dispose = wireTransientScrollbar(host as unknown as HTMLElement, {
    activeClass: ACTIVE,
    hostClass: "shell",
    onScrollbar: options.onScrollbar ?? (() => false),
    revealOnHover: options.revealOnHover,
  });
  return { host, win, dispose, revealed: () => host.classes.has(ACTIVE) };
}

describe("wireTransientScrollbar", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => {
    vi.useRealTimers();
    vi.unstubAllGlobals();
  });

  it("reveals around scrolling and retracts once the pointer goes quiet", () => {
    const { host, revealed, dispose } = harness();

    host.dispatch("wheel");
    expect(revealed()).toBe(true);

    vi.advanceTimersByTime(SCROLLBAR_IDLE_MS - 1);
    expect(revealed()).toBe(true);
    vi.advanceTimersByTime(1);
    expect(revealed()).toBe(false);

    dispose();
  });

  it("holds the bar up for the whole of a thumb drag", () => {
    const { host, win, revealed, dispose } = harness({ onScrollbar: () => true });

    host.dispatch("pointerdown");
    expect(revealed()).toBe(true);

    // The idle timer must not retract the bar out from under the pointer
    // dragging it, however long the drag lasts.
    vi.advanceTimersByTime(SCROLLBAR_IDLE_MS * 4);
    expect(revealed()).toBe(true);

    win.dispatch("pointerup");
    expect(revealed()).toBe(true);
    vi.advanceTimersByTime(SCROLLBAR_IDLE_MS);
    expect(revealed()).toBe(false);

    dispose();
  });

  it.each(["pointerup", "pointercancel", "blur"])(
    "ends a drag on %s outside the host so the bar cannot be stranded",
    (event) => {
      const { host, win, revealed, dispose } = harness({ onScrollbar: () => true });

      host.dispatch("pointerdown");
      vi.advanceTimersByTime(SCROLLBAR_IDLE_MS * 2);
      expect(revealed()).toBe(true);

      win.dispatch(event);
      vi.advanceTimersByTime(SCROLLBAR_IDLE_MS);
      expect(revealed()).toBe(false);

      dispose();
    },
  );

  it("leaves a press that missed the scrollbar alone", () => {
    const { host, revealed, dispose } = harness({ onScrollbar: () => false });

    host.dispatch("pointerdown");
    expect(revealed()).toBe(false);

    host.dispatch("wheel");
    vi.advanceTimersByTime(SCROLLBAR_IDLE_MS);
    expect(revealed()).toBe(false);

    dispose();
  });

  it("reveals on hover only when the host asked for it", () => {
    const without = harness({ onScrollbar: () => true });
    without.host.dispatch("pointermove");
    expect(without.revealed()).toBe(false);
    without.dispose();

    const withHover = harness({ onScrollbar: () => true, revealOnHover: true });
    withHover.host.dispatch("pointermove");
    expect(withHover.revealed()).toBe(true);
    withHover.dispose();
  });

  it("reveals on the events the host nominates", () => {
    const host = new FakeTarget();
    const win = new FakeTarget();
    vi.stubGlobal("window", {
      setTimeout: (fn: () => void, ms: number) => setTimeout(fn, ms),
      clearTimeout: (id: number) => clearTimeout(id),
      addEventListener: win.addEventListener,
      removeEventListener: win.removeEventListener,
    });
    const dispose = wireTransientScrollbar(host as unknown as HTMLElement, {
      activeClass: ACTIVE,
      onScrollbar: () => false,
      revealOn: ["scroll"],
    });

    host.dispatch("wheel");
    expect(host.classes.has(ACTIVE)).toBe(false);
    host.dispatch("scroll");
    expect(host.classes.has(ACTIVE)).toBe(true);

    dispose();
  });

  it("drops every class and listener on dispose, mid-drag included", () => {
    const { host, win, dispose } = harness({ onScrollbar: () => true, revealOnHover: true });

    expect(host.classes.has("shell")).toBe(true);
    host.dispatch("pointerdown");

    dispose();

    expect(host.classes.size).toBe(0);
    expect(host.listenerCount()).toBe(0);
    expect(win.listenerCount()).toBe(0);
  });
});

describe("overNativeVerticalScrollbar", () => {
  const host = (overflowing: boolean) => ({
    scrollHeight: overflowing ? 900 : 100,
    clientHeight: 400,
    clientWidth: 292,
    getBoundingClientRect: () => ({ left: 8 }),
  } as unknown as HTMLElement);

  it("claims a pointer past the content box", () => {
    expect(overNativeVerticalScrollbar(host(true), { clientX: 304 } as PointerEvent)).toBe(true);
  });

  it("leaves a pointer over the content alone", () => {
    expect(overNativeVerticalScrollbar(host(true), { clientX: 299 } as PointerEvent)).toBe(false);
  });

  it("claims nothing while the list fits, because there is no bar to grab", () => {
    expect(overNativeVerticalScrollbar(host(false), { clientX: 304 } as PointerEvent)).toBe(false);
  });
});
