import { describe, expect, it } from "vitest";
import { computeScrollClearance } from "./useFooterClearance";

describe("computeScrollClearance", () => {
  it("adds spacing to the footer height", () => {
    expect(computeScrollClearance(80)).toBe(96);
  });

  it("returns only spacing when footer height is zero", () => {
    expect(computeScrollClearance(0)).toBe(16);
  });

  it("accepts a custom spacing value", () => {
    expect(computeScrollClearance(80, 24)).toBe(104);
  });

  it("handles a tall submit bar with safe-area padding", () => {
    // Submit bar: 12pt top + 48pt button + 20pt bottom safe area = 80pt
    expect(computeScrollClearance(80, 16)).toBe(96);
  });
});
