import { describe, expect, it } from "vitest";
import { computeKeyboardOverlap } from "./keyboardOverlap";

describe("computeKeyboardOverlap", () => {
  it("returns zero when keyboard is below the view", () => {
    expect(computeKeyboardOverlap(800, 900)).toBe(0);
  });

  it("returns zero when keyboard top equals view bottom", () => {
    expect(computeKeyboardOverlap(800, 800)).toBe(0);
  });

  it("returns the overlap when keyboard covers part of the view", () => {
    expect(computeKeyboardOverlap(800, 500)).toBe(300);
  });

  it("handles a keyboard that covers the entire view", () => {
    expect(computeKeyboardOverlap(800, 0)).toBe(800);
  });

  it("handles fractional coordinates", () => {
    expect(computeKeyboardOverlap(812.5, 500.25)).toBeCloseTo(312.25);
  });
});
