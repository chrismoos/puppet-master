import { describe, expect, it } from "vitest";
import { normalizeWorkspaceName } from "./WorkspaceDialog";

describe("workspace names", () => {
  it("trims names before create or rename", () => {
    expect(normalizeWorkspaceName("  release watch  ")).toBe("release watch");
    expect(normalizeWorkspaceName("   ")).toBe("");
  });
});
