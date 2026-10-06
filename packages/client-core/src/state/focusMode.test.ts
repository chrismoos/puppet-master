import { describe, expect, it } from "vitest";
import { focusModeActionForKey, focusModeReducer } from "./focusMode";

describe("focus mode", () => {
  it("toggles in both directions", () => {
    expect(focusModeReducer(false, "toggle")).toBe(true);
    expect(focusModeReducer(true, "toggle")).toBe(false);
  });

  it("exits when requested", () => {
    expect(focusModeReducer(true, "exit")).toBe(false);
    expect(focusModeReducer(false, "exit")).toBe(false);
  });

  it("enters when requested", () => {
    expect(focusModeReducer(false, "enter")).toBe(true);
    expect(focusModeReducer(true, "enter")).toBe(true);
  });

  it("maps Escape to exit without consuming other keys", () => {
    expect(focusModeActionForKey("Escape")).toBe("exit");
    expect(focusModeActionForKey("Enter")).toBeNull();
  });
});
