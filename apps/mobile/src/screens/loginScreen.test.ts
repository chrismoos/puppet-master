import { describe, expect, it } from "vitest";

describe("login form validation", () => {
  function canSubmit(url: string, username: string, password: string, busy: boolean): boolean {
    const normalizedUrl = url.trim().replace(/\/+$/, "");
    return Boolean(normalizedUrl && username && password) && !busy;
  }

  it("requires all three fields to be non-empty", () => {
    expect(canSubmit("https://pm.test", "user", "pass", false)).toBe(true);
    expect(canSubmit("", "user", "pass", false)).toBe(false);
    expect(canSubmit("https://pm.test", "", "pass", false)).toBe(false);
    expect(canSubmit("https://pm.test", "user", "", false)).toBe(false);
  });

  it("trims and strips trailing slashes from the URL", () => {
    expect(canSubmit("  https://pm.test///  ", "u", "p", false)).toBe(true);
  });

  it("disables submit while busy", () => {
    expect(canSubmit("https://pm.test", "u", "p", true)).toBe(false);
  });

  it("rejects a whitespace-only URL", () => {
    expect(canSubmit("   ", "u", "p", false)).toBe(false);
  });
});

describe("login URL normalisation", () => {
  function normalize(url: string): string {
    return url.trim().replace(/\/+$/, "");
  }

  it("strips trailing slashes", () => {
    expect(normalize("https://pm.test///")).toBe("https://pm.test");
  });

  it("trims whitespace", () => {
    expect(normalize("  https://pm.test  ")).toBe("https://pm.test");
  });
});
