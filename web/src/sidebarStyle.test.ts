import { describe, expect, it } from "vitest";
import { parseSidebarStyle } from "./sidebarStyle";

describe("the stored sidebar style", () => {
  it("reads the two styles the daemon stores", () => {
    expect(parseSidebarStyle('"dropdown"')).toBe("dropdown");
    expect(parseSidebarStyle('"rail"')).toBe("rail");
  });

  it("uses the dropdown when nothing valid is stored", () => {
    for (const stored of [undefined, '"Rail"', "rail", "null", "{}", "not json"]) {
      expect(parseSidebarStyle(stored), String(stored)).toBe("dropdown");
    }
  });
});
