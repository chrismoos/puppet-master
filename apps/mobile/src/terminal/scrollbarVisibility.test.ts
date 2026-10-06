import { describe, expect, it } from "vitest";

import { ScrollbarVisibility } from "./scrollbarVisibility";

const CONFIG = { fadeDelayMs: 600, fadeDurationMs: 250 };

describe("ScrollbarVisibility", () => {
  it("starts hidden", () => {
    const vis = new ScrollbarVisibility(CONFIG);
    expect(vis.phase(0)).toBe("hidden");
    expect(vis.opacity(0)).toBe(0);
  });

  it("shows on scroll activity and stays visible while engaged", () => {
    const vis = new ScrollbarVisibility(CONFIG);
    vis.activity();
    expect(vis.phase(0)).toBe("visible");
    expect(vis.phase(10_000)).toBe("visible");
    expect(vis.opacity(10_000)).toBe(1);
  });

  it("stays visible through the idle delay after settling", () => {
    const vis = new ScrollbarVisibility(CONFIG);
    vis.activity();
    vis.settle(1_000);
    expect(vis.phase(1_000)).toBe("visible");
    expect(vis.phase(1_599)).toBe("visible");
  });

  it("fades after the delay and hides once the fade completes", () => {
    const vis = new ScrollbarVisibility(CONFIG);
    vis.activity();
    vis.settle(1_000);
    expect(vis.phase(1_600)).toBe("fading");
    expect(vis.opacity(1_600)).toBe(1);
    expect(vis.opacity(1_725)).toBeCloseTo(0.5);
    expect(vis.phase(1_850)).toBe("hidden");
    expect(vis.opacity(1_850)).toBe(0);
  });

  it("returns to fully visible when activity resumes mid-fade", () => {
    const vis = new ScrollbarVisibility(CONFIG);
    vis.activity();
    vis.settle(1_000);
    expect(vis.phase(1_700)).toBe("fading");
    vis.activity();
    expect(vis.phase(1_700)).toBe("visible");
    expect(vis.opacity(1_700)).toBe(1);
  });

  it("ignores settle when nothing is engaged", () => {
    const vis = new ScrollbarVisibility(CONFIG);
    vis.settle(500);
    expect(vis.phase(500)).toBe("hidden");
    vis.activity();
    vis.settle(1_000);
    vis.settle(2_000);
    expect(vis.phase(1_599)).toBe("visible");
    expect(vis.phase(1_700)).toBe("fading");
  });
});
