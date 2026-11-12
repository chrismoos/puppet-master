import { describe, expect, it } from "vitest";
import { parseSidebarStyle } from "./sidebarStyle";

describe("the stored sidebar style", () => {
  it("reads the two styles the daemon stores", () => {
    expect(parseSidebarStyle('"dropdown"')).toBe("dropdown");
    expect(parseSidebarStyle('"rail"')).toBe("rail");
  });

  it("uses the rail when nothing valid is stored", () => {
    for (const stored of [undefined, '"Dropdown"', "dropdown", "null", "{}", "not json"]) {
      expect(parseSidebarStyle(stored), String(stored)).toBe("rail");
    }
  });
});
