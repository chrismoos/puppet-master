import { describe, expect, it } from "vitest";
import { anchorBelow } from "./menuAnchor";

describe("where the menu opens", () => {
  it("sits just below the button, lined up with its right edge", () => {
    expect(anchorBelow({ x: 340, y: 59, width: 28, height: 28 }, 390)).toEqual({ top: 93, right: 22 });
  });

  it("follows the button wherever it is, as in landscape", () => {
    expect(anchorBelow({ x: 780, y: 12, width: 28, height: 28 }, 844)).toEqual({ top: 46, right: 36 });
  });

  it("never sits past the screen's right edge", () => {
    expect(anchorBelow({ x: 380, y: 10, width: 28, height: 28 }, 390).right).toBe(0);
  });
});
