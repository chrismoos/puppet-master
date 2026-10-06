import { describe, expect, it } from "vitest";
import { BUILTIN_TERMINAL_THEME, exportNativeTerminalTheme } from "@puppet-master/client-core/theme/terminalTheme";
import {
  MIN_PASSWORD_CHARS,
  idleThresholdMessage,
  parseIdleMinutes,
  parseImportedTheme,
  passwordProblem,
  readThemeFile,
} from "./AccountSettings";
import { PUSH_WEB_IDLE_MINUTES_MAX } from "../api/userSettings";

describe("terminal theme import helpers", () => {
  it("routes by contents rather than filename while keeping strict validation", () => {
    expect(parseImportedTheme(exportNativeTerminalTheme(BUILTIN_TERMINAL_THEME), "PuppetMaster").format)
      .toBe("native");
    const ghostty = parseImportedTheme("background = #000000", "midnight.json");
    expect(ghostty.format).toBe("ghostty");
    expect(ghostty.theme.name).toBe("midnight");
    expect(ghostty.warnings.join(" ")).toMatch(/Filled missing colors/);
    expect(parseImportedTheme("background = #000000", "midnight.conf").format).toBe("ghostty");
    expect(() => parseImportedTheme('{"kind":', "broken.conf")).toThrow(/Malformed JSON/);
    expect(() => parseImportedTheme("not a theme", "notes.txt")).toThrow(/Malformed JSON/);
  });

  it("rejects oversized files before reading them", async () => {
    const blob = new Blob([new Uint8Array(64 * 1024 + 1)]);
    await expect(readThemeFile(blob)).rejects.toThrow(/64 KiB/);
  });

  it("rejects non-UTF-8 imports before selecting a parser", async () => {
    await expect(readThemeFile(new Blob([new Uint8Array([0xff])]))).rejects.toThrow(/UTF-8/);
  });
});

describe("push idle threshold helpers", () => {
  it("accepts whole minutes inside the range the daemon accepts", () => {
    expect(parseIdleMinutes("3")).toBe(3);
    expect(parseIdleMinutes(" 0 ")).toBe(0);
    expect(parseIdleMinutes(String(PUSH_WEB_IDLE_MINUTES_MAX))).toBe(PUSH_WEB_IDLE_MINUTES_MAX);
  });

  it("refuses what the daemon would refuse, so the page never sends it", () => {
    for (const input of ["", "-1", "1.5", "3m", String(PUSH_WEB_IDLE_MINUTES_MAX + 1)]) {
      expect(parseIdleMinutes(input), input).toBeNull();
    }
  });

  it("says push is no longer held back at all when the gate is off", () => {
    expect(idleThresholdMessage(0)).toContain("even while you are working here");
  });

  it("names the threshold it saved", () => {
    expect(idleThresholdMessage(1)).toContain("a minute");
    expect(idleThresholdMessage(7)).toContain("7 minutes");
  });
});

describe("passwordProblem", () => {
  const long = "correct horse battery";

  it("accepts a well-formed change", () => {
    expect(passwordProblem("hunter2hunter2", long, long)).toBeNull();
  });

  it("requires every field", () => {
    expect(passwordProblem("", long, long)).toBe("Fill in every field.");
    expect(passwordProblem("old", "", long)).toBe("Fill in every field.");
    expect(passwordProblem("old", long, "")).toBe("Fill in every field.");
  });

  it("catches a mistyped confirmation before the round trip", () => {
    expect(passwordProblem("hunter2hunter2", long, `${long}x`)).toBe(
      "The new passwords do not match.",
    );
  });

  it("holds the same length floor the daemon does", () => {
    const short = "a".repeat(MIN_PASSWORD_CHARS - 1);
    expect(passwordProblem("hunter2hunter2", short, short)).toBe(
      `The new password must be at least ${MIN_PASSWORD_CHARS} characters.`,
    );
    const exact = "a".repeat(MIN_PASSWORD_CHARS);
    expect(passwordProblem("hunter2hunter2", exact, exact)).toBeNull();
  });

  // Counted in characters, so a short passphrase of wide glyphs is not
  // waved through on byte length.
  it("counts characters rather than code units", () => {
    const emoji = "\u{1F511}".repeat(MIN_PASSWORD_CHARS - 1);
    expect(passwordProblem("hunter2hunter2", emoji, emoji)).toBe(
      `The new password must be at least ${MIN_PASSWORD_CHARS} characters.`,
    );
  });

  it("rejects a change that changes nothing", () => {
    expect(passwordProblem(long, long, long)).toBe(
      "The new password must differ from the current one.",
    );
  });
});
