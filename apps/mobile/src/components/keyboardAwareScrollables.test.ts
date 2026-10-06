import { describe, expect, it } from "vitest";
import { keyboardScrollDefaults } from "./keyboardScrollDefaults";

describe("keyboard scroll defaults", () => {
  it("enables native keyboard insets on iOS", () => {
    expect(keyboardScrollDefaults(true)).toEqual({
      keyboardShouldPersistTaps: "handled",
      automaticallyAdjustKeyboardInsets: true,
    });
  });

  it("keeps tap handling without iOS-only insets on other platforms", () => {
    expect(keyboardScrollDefaults(false)).toEqual({
      keyboardShouldPersistTaps: "handled",
    });
  });
});
