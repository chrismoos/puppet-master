import { describe, expect, it } from "vitest";

import { shellTabLabel } from "./label";

describe("shellTabLabel", () => {
  it("uses a live terminal title instead of the generic shell title", () => {
    expect(shellTabLabel(9n, "Shell", "cargo nextest run")).toBe("cargo nextest run");
  });

  it("truncates long titles and gives untitled shells an identity", () => {
    expect(shellTabLabel(9n, "Shell")).toBe("shell 9");
    const label = shellTabLabel(9n, "Shell", "a".repeat(80));
    expect([...label]).toHaveLength(32);
    expect(label.endsWith("…")).toBe(true);
  });

  it("uses a non-generic configured title when no live title exists", () => {
    expect(shellTabLabel(5n, "my-build")).toBe("my-build");
  });

  it("falls back to shell {id} when everything is empty", () => {
    expect(shellTabLabel(7n, "")).toBe("shell 7");
  });
});
