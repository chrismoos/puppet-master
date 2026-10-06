import { describe, expect, it } from "vitest";
import { nextPlanSelection, customOptionLabel } from "./selectionHelpers";

describe("nextPlanSelection", () => {
  it("replaces a single selection with the new key", () => {
    expect(nextPlanSelection("single", ["one"], "two")).toEqual(["two"]);
  });

  it("selects the first option in single mode", () => {
    expect(nextPlanSelection("single", [], "first")).toEqual(["first"]);
  });

  it("toggles keys in multiple mode — add", () => {
    expect(nextPlanSelection("multiple", ["one"], "two")).toEqual(["one", "two"]);
  });

  it("toggles keys in multiple mode — remove", () => {
    expect(nextPlanSelection("multiple", ["one", "two"], "one")).toEqual(["two"]);
  });

  it("starts fresh in multiple mode", () => {
    expect(nextPlanSelection("multiple", [], "first")).toEqual(["first"]);
  });

  it("returns empty for dialogue mode", () => {
    expect(nextPlanSelection("dialogue", ["x"], "y")).toEqual([]);
  });
});

describe("customOptionLabel", () => {
  it("extracts the first non-empty line trimmed", () => {
    expect(customOptionLabel("  Hybrid store\nUse both systems.  ")).toBe("Hybrid store");
  });

  it("returns empty for empty input", () => {
    expect(customOptionLabel("")).toBe("");
  });

  it("handles single line", () => {
    expect(customOptionLabel("Just one line")).toBe("Just one line");
  });

  it("skips leading blank lines", () => {
    expect(customOptionLabel("\n\n  Real label\ndetail")).toBe("Real label");
  });
});
