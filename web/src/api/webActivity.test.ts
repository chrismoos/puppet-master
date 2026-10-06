import { describe, expect, it } from "vitest";
import {
  ACTIVITY_EVENTS,
  ACTIVITY_REPORT_INTERVAL_MS,
  startActivityReporter,
} from "./webActivity";

function harness(options: { visible?: boolean } = {}) {
  const listeners = new Map<string, { fire: () => void; options: AddEventListenerOptions }>();
  const reports: number[] = [];
  let visible = options.visible ?? true;
  let clock = 1_000;
  const stop = startActivityReporter({
    target: {
      addEventListener: (type, listener, listenerOptions) => {
        listeners.set(type, { fire: listener, options: listenerOptions });
      },
      removeEventListener: (type) => {
        listeners.delete(type);
      },
    },
    visible: () => visible,
    report: () => reports.push(clock),
    now: () => clock,
  });
  return {
    listeners,
    reports,
    stop,
    interact: (event: string = "keydown") => listeners.get(event)?.fire(),
    advance: (ms: number) => {
      clock += ms;
    },
    setVisible: (next: boolean) => {
      visible = next;
    },
  };
}

describe("startActivityReporter", () => {
  it("listens for the four interaction events in the capture phase, passively", () => {
    const { listeners } = harness();

    expect([...listeners.keys()]).toEqual([...ACTIVITY_EVENTS]);
    for (const [type, { options }] of listeners) {
      expect(options, type).toEqual({ capture: true, passive: true });
    }
  });

  it("reports the first interaction immediately", () => {
    const { interact, reports } = harness();

    interact();

    expect(reports).toEqual([1_000]);
  });

  it("reports at most once per interval however much the user types", () => {
    const { interact, advance, reports } = harness();

    interact();
    for (let i = 0; i < 50; i += 1) {
      advance(100);
      interact();
    }

    expect(reports).toEqual([1_000]);

    advance(ACTIVITY_REPORT_INTERVAL_MS);
    interact();

    expect(reports).toEqual([1_000, 1_000 + ACTIVITY_REPORT_INTERVAL_MS + 5_000]);
  });

  it("stays silent while the page is hidden, however much happens in it", () => {
    const { interact, advance, reports, setVisible } = harness({ visible: false });

    for (const event of ACTIVITY_EVENTS) {
      interact(event);
      advance(ACTIVITY_REPORT_INTERVAL_MS * 2);
    }

    expect(reports).toEqual([]);

    setVisible(true);
    interact();

    expect(reports.length).toBe(1);
  });

  it("reports every kind of interaction, including one xterm would consume", () => {
    for (const event of ACTIVITY_EVENTS) {
      const { interact, reports } = harness();
      interact(event);
      expect(reports, event).toEqual([1_000]);
    }
  });

  it("stops listening when stopped", () => {
    const { listeners, stop } = harness();

    stop();

    expect(listeners.size).toBe(0);
  });
});
