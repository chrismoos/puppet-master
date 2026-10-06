import { describe, expect, it } from "vitest";
import {
  ANSI_COLOR_KEYS,
  BUILTIN_TERMINAL_THEME,
  TERMINAL_THEME_MAX_BYTES,
  ThemeParseError,
  exportNativeTerminalTheme,
  parseGhosttyTerminalTheme,
  parseNativeTerminalTheme,
  terminalThemeToXterm,
} from "./terminalTheme";
import schemaFixture from "./terminal-theme-v1.fixture.json?raw";

describe("native Puppet Master terminal themes", () => {
  it("matches the same v1 schema fixture validated by Rust", () => {
    const theme = parseNativeTerminalTheme(schemaFixture).theme;
    expect(theme.name).toBe("Schema parity fixture");
    expect(theme.colors.selectionBackground).toBe("#2b3548cc");
    expect(theme.colors.overviewRulerBorder).toBe("#667788");
  });

  it("round-trips every supported field and normalizes strict sRGB colors", () => {
    const input = {
      ...BUILTIN_TERMINAL_THEME,
      name: "Exact native",
      author: "Example",
      license: "MIT",
      colors: {
        ...BUILTIN_TERMINAL_THEME.colors,
        foreground: "#C9CEDA",
        selectionBackground: "#2B3548CC",
        selectionInactiveBackground: "#11182780",
        scrollbarSliderBackground: "#334455",
        scrollbarSliderHoverBackground: "#445566",
        scrollbarSliderActiveBackground: "#556677",
        overviewRulerBorder: "#667788",
        extendedAnsi: Array.from({ length: 240 }, (_, index) =>
          `#${index.toString(16).padStart(6, "0")}`),
      },
    };
    const parsed = parseNativeTerminalTheme(JSON.stringify(input)).theme;
    expect(parsed.colors.foreground).toBe("#c9ceda");
    expect(parsed.colors.selectionBackground).toBe("#2b3548cc");
    expect(parsed.colors.extendedAnsi).toHaveLength(240);
    expect(parseNativeTerminalTheme(exportNativeTerminalTheme(parsed)).theme).toEqual(parsed);
    expect(terminalThemeToXterm(parsed)).toEqual(parsed.colors);
  });

  it("rejects unknown fields, malformed colors, sparse extended ANSI and oversized input", () => {
    const unknown = { ...BUILTIN_TERMINAL_THEME, colors: { ...BUILTIN_TERMINAL_THEME.colors, link: "#ffffff" } };
    expect(() => parseNativeTerminalTheme(JSON.stringify(unknown))).toThrow(/Unknown colors field/);
    const alphaBackground = { ...BUILTIN_TERMINAL_THEME, colors: { ...BUILTIN_TERMINAL_THEME.colors, background: "#000000ff" } };
    expect(() => parseNativeTerminalTheme(JSON.stringify(alphaBackground))).toThrow(/background/);
    const sparse = { ...BUILTIN_TERMINAL_THEME, colors: { ...BUILTIN_TERMINAL_THEME.colors, extendedAnsi: ["#000000"] } };
    expect(() => parseNativeTerminalTheme(JSON.stringify(sparse))).toThrow(/all 240 colors/);
    expect(() => parseNativeTerminalTheme(" ".repeat(TERMINAL_THEME_MAX_BYTES + 1))).toThrow(/64 KiB/);
  });
});

describe("Ghostty color theme imports", () => {
  it("maps a complete core and ANSI 0-255 palette", () => {
    const core = [
      "foreground = #c9ceda", "background = #0b0e14",
      "cursor-color = #ffb224", "cursor-text = #0b0e14",
      "selection-foreground = #eef1f6", "selection-background = #2b3548cc",
    ];
    const palette = Array.from({ length: 256 }, (_, index) =>
      `palette = ${index}=#${index.toString(16).padStart(6, "0")}`);
    const result = parseGhosttyTerminalTheme([...core, ...palette].join("\n"), "Complete");
    expect(result.theme.name).toBe("Complete");
    expect(result.theme.colors.black).toBe("#000000");
    expect(result.theme.colors.brightWhite).toBe("#00000f");
    expect(result.theme.colors.extendedAnsi).toHaveLength(240);
    expect(result.theme.colors.extendedAnsi?.[0]).toBe("#000010");
    expect(result.warnings).toEqual([]);
  });

  it("fills missing core, diagnoses duplicates and ignores partial extended colors", () => {
    const result = parseGhosttyTerminalTheme([
      "background = #111111",
      "background = #222222",
      "palette = 0=#010203",
      "palette = 16=#abcdef",
      "include = ~/.config/ghostty/secret",
      "font-family = Example",
    ].join("\n"));
    expect(result.theme.colors.background).toBe("#222222");
    expect(result.theme.colors.black).toBe("#010203");
    expect(result.theme.colors.foreground).toBe(BUILTIN_TERMINAL_THEME.colors.foreground);
    expect(result.theme.colors.extendedAnsi).toBeUndefined();
    expect(result.warnings.join("\n")).toMatch(/duplicate background/);
    expect(result.warnings.join("\n")).toMatch(/Filled missing colors/);
    expect(result.warnings.join("\n")).toMatch(/partial extended palette/);
    expect(result.warnings.join("\n")).toMatch(/ignored Ghostty key "include"/);
  });

  it("rejects malformed known colors and palette entries", () => {
    expect(() => parseGhosttyTerminalTheme("foreground = red")).toThrow(ThemeParseError);
    expect(() => parseGhosttyTerminalTheme("palette = nope=#ffffff")).toThrow(/palette index/);
    expect(() => parseGhosttyTerminalTheme("palette = 1")).toThrow(/INDEX/);
  });

  it("maps exactly the sixteen named ANSI entries", () => {
    const input = ANSI_COLOR_KEYS.map((_, index) => `palette = ${index}=#123456`).join("\n");
    const theme = parseGhosttyTerminalTheme(input).theme;
    for (const key of ANSI_COLOR_KEYS) expect(theme.colors[key]).toBe("#123456");
  });
});
