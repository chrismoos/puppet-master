import { describe, expect, it } from "vitest";
import { toggleMenu } from "./menuState";

describe("toggleMenu", () => {
  it("opens a menu from a closed state", () => {
    expect(toggleMenu(null, "bucket:1")).toBe("bucket:1");
  });

  it("closes the menu when its own key is toggled", () => {
    expect(toggleMenu("bucket:1", "bucket:1")).toBeNull();
  });

  it("replaces a different open menu so only one is open", () => {
    expect(toggleMenu("bucket:1", "project:2")).toBe("project:2");
  });
});
