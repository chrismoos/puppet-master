import { describe, expect, it } from "vitest";
import {
  ITEM_BODY_MAX_CHARACTERS,
  itemBodyCharacterCount,
  itemBodyLengthError,
} from "./itemBodyLimit";

describe("item body length", () => {
  it("counts Unicode scalar values instead of UTF-16 code units", () => {
    expect(itemBodyCharacterCount("A😀é")).toBe(3);
  });

  it("accepts the boundary and describes one character over", () => {
    expect(itemBodyLengthError("x".repeat(ITEM_BODY_MAX_CHARACTERS))).toBeNull();
    expect(itemBodyLengthError("x".repeat(ITEM_BODY_MAX_CHARACTERS + 1))).toContain("65,537");
    expect(itemBodyLengthError("x".repeat(ITEM_BODY_MAX_CHARACTERS + 1))).toContain("65,536");
  });
});
