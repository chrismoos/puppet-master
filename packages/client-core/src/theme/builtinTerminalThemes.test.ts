import { describe, expect, it } from "vitest";
import {
  BUILTIN_TERMINAL_THEMES,
  builtinTerminalThemeMatching,
  findBuiltinTerminalTheme,
} from "./builtinTerminalThemes";
import {
  BUILTIN_TERMINAL_THEME,
  CORE_COLOR_KEYS,
  contrastRatio,
  contrastWarnings,
  exportNativeTerminalTheme,
  parseNativeTerminalTheme,
  terminalThemeAppearance,
} from "./terminalTheme";

describe("bundled terminal themes", () => {
  it("ships palettes that survive the strict native schema unchanged", () => {
    for (const theme of BUILTIN_TERMINAL_THEMES) {
      const round = parseNativeTerminalTheme(exportNativeTerminalTheme(theme)).theme;
      expect(round).toEqual(theme);
    }
  });

  it("clears the app's own contrast check on every bundled palette", () => {
    for (const theme of BUILTIN_TERMINAL_THEMES) {
      expect(
        contrastWarnings(theme),
        `${theme.name} foreground/background is ${contrastRatio(theme.colors.foreground, theme.colors.background).toFixed(2)}:1`,
      ).toEqual([]);
    }
  });

  it("leads with the Puppet Master default and names each palette once", () => {
    expect(BUILTIN_TERMINAL_THEMES[0]).toBe(BUILTIN_TERMINAL_THEME);
    const names = BUILTIN_TERMINAL_THEMES.map((theme) => theme.name);
    expect(new Set(names).size).toBe(names.length);
    expect(names).toContain("Dracula");
  });

  it("carries the upstream author and licence on every borrowed palette", () => {
    for (const theme of BUILTIN_TERMINAL_THEMES) {
      if (theme === BUILTIN_TERMINAL_THEME) continue;
      expect(theme.author, theme.name).toBeTruthy();
      expect(theme.license, theme.name).toMatch(/Copyright/);
    }
  });

  it("bundles a genuinely light palette, not only dark ones", () => {
    const light = BUILTIN_TERMINAL_THEMES.filter((theme) => terminalThemeAppearance(theme) === "light");
    expect(light.map((theme) => theme.name)).toContain("Solarized Light");
    for (const theme of light) {
      expect(
        contrastRatio(theme.colors.background, "#ffffff"),
        `${theme.name} background is too dark to be a light theme`,
      ).toBeLessThan(1.5);
    }
    expect(BUILTIN_TERMINAL_THEMES.some((theme) => terminalThemeAppearance(theme) === "dark")).toBe(true);
  });

  it("identifies a bundled palette by its colours rather than its name", () => {
    for (const theme of BUILTIN_TERMINAL_THEMES) {
      expect(builtinTerminalThemeMatching({ ...theme, name: "Renamed" })).toBe(theme);
    }
    const impostor = {
      ...BUILTIN_TERMINAL_THEMES[1],
      colors: { ...BUILTIN_TERMINAL_THEMES[1].colors, brightCyan: "#123456" },
    };
    expect(builtinTerminalThemeMatching(impostor)).toBeUndefined();
  });

  it("looks a bundled palette up by name and refuses an unknown one", () => {
    expect(findBuiltinTerminalTheme("Solarized Light")?.colors.background).toBe("#fdf6e3");
    expect(findBuiltinTerminalTheme("solarized light")).toBeUndefined();
  });

  it("defines every core colour on every bundled palette", () => {
    for (const theme of BUILTIN_TERMINAL_THEMES) {
      for (const key of CORE_COLOR_KEYS) {
        expect(theme.colors[key], `${theme.name}.${key}`).toMatch(/^#[0-9a-f]{6}([0-9a-f]{2})?$/);
      }
    }
  });
});
