import { describe, expect, it } from "vitest";

import { AccessoryRowVisibility } from "./accessoryVisibility";

describe("AccessoryRowVisibility", () => {
  it("is hidden by default", () => {
    expect(new AccessoryRowVisibility().visible()).toBe(false);
  });

  it("follows the keyboard", () => {
    const row = new AccessoryRowVisibility();
    row.keyboardShown();
    expect(row.visible()).toBe(true);
    row.keyboardHidden();
    expect(row.visible()).toBe(false);
  });

  it("can be summoned without the keyboard and dismissed again", () => {
    const row = new AccessoryRowVisibility();
    expect(row.toggle()).toBe(true);
    expect(row.visible()).toBe(true);
    expect(row.toggle()).toBe(false);
    expect(row.visible()).toBe(false);
  });

  it("stays dismissed through keyboard frame changes after a manual dismissal", () => {
    const row = new AccessoryRowVisibility();
    row.keyboardShown();
    row.toggle();
    expect(row.visible()).toBe(false);
    row.keyboardShown();
    expect(row.visible()).toBe(false);
  });

  it("reappears with the next keyboard after being dismissed", () => {
    const row = new AccessoryRowVisibility();
    row.keyboardShown();
    row.toggle();
    row.keyboardHidden();
    expect(row.visible()).toBe(false);
    row.keyboardShown();
    expect(row.visible()).toBe(true);
  });

  it("hides with the keyboard after being summoned manually", () => {
    const row = new AccessoryRowVisibility();
    row.toggle();
    row.keyboardShown();
    expect(row.visible()).toBe(true);
    row.keyboardHidden();
    expect(row.visible()).toBe(false);
  });

  it("can be re-summoned while the keyboard is up", () => {
    const row = new AccessoryRowVisibility();
    row.keyboardShown();
    row.toggle();
    expect(row.toggle()).toBe(true);
    expect(row.visible()).toBe(true);
  });
});
