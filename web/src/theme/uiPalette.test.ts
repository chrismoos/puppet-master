import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, it } from "vitest";
import { codeSurfaceVariables } from "@puppet-master/client-core/theme/codeSurface";
import { BUILTIN_TERMINAL_THEME } from "@puppet-master/client-core/theme/terminalTheme";
import { UI_THEMES, type UiTheme } from "@puppet-master/client-core/theme/uiTheme";
import { parseAppearancePalettes, themeBlocks, uiContrastWarnings } from "./uiPalette";

const CSS = readFileSync(fileURLToPath(new URL("../styles.css", import.meta.url)), "utf8");
const CONNECTION_CSS = readFileSync(fileURLToPath(new URL("../views/ConnectionsPanel.css", import.meta.url)), "utf8");

describe.each(UI_THEMES)("application appearance palettes: %s", (theme) => {
  const palettes = parseAppearancePalettes(CSS, theme);

  it("defines both appearances from one set of light-dark() pairs", () => {
    expect(Object.keys(palettes.light).length).toBeGreaterThan(30);
    expect(Object.keys(palettes.light)).toEqual(Object.keys(palettes.dark));
    expect(palettes.light["--bg"]).not.toBe(palettes.dark["--bg"]);
  });

  it("defines every colour the default palette defines", () => {
    const standard = parseAppearancePalettes(CSS, "standard");
    expect(Object.keys(palettes.light).sort()).toEqual(Object.keys(standard.light).sort());
  });

  it("clears the contrast floors in light", () => {
    expect(uiContrastWarnings(palettes.light)).toEqual([]);
  });

  it("clears the contrast floors in dark", () => {
    expect(uiContrastWarnings(palettes.dark)).toEqual([]);
  });

  it("grounds light on a light surface and dark on a dark one", () => {
    expect(luminance(palettes.light["--bg"])).toBeGreaterThan(0.7);
    expect(luminance(palettes.dark["--bg"])).toBeLessThan(0.05);
    expect(luminance(palettes.light["--text"])).toBeLessThan(0.1);
    expect(luminance(palettes.dark["--text"])).toBeGreaterThan(0.5);
  });
});

describe("application theme typography", () => {
  const declared = (theme: UiTheme, token: string) =>
    themeBlocks(CSS, theme)
      .map((block) => block.match(new RegExp(`${token}:\\s*([^;]+);`))?.[1])
      .find(Boolean);

  it("keeps Midnight on the mono face with uppercase tracked controls", () => {
    expect(declared("standard", "--font-ui")).toBe("var(--mono)");
    expect(declared("standard", "--btn-transform")).toBe("uppercase");
    expect(declared("standard", "--btn-tracking")).toBe("0.08em");
    expect(declared("standard", "--badge-transform")).toBe("uppercase");
  });

  it.each(["graphite", "studio"] as const)("sets %s in Inter with sentence-case controls", (theme) => {
    expect(declared(theme, "--font-ui")).toBe("var(--sans)");
    expect(declared(theme, "--btn-transform")).toBe("none");
    expect(declared(theme, "--btn-tracking")).toBe("0");
    expect(declared(theme, "--badge-transform")).toBe("none");
    expect(declared("standard", "--sans")).toMatch(/^"Inter Variable"/);
  });

  it("styles buttons and badges from the tokens rather than per-theme rules", () => {
    expect(CSS).toMatch(/\.btn \{[^}]*text-transform: var\(--btn-transform\);[^}]*letter-spacing: var\(--btn-tracking\);/);
    expect(CSS).toMatch(/\.badge \{[^}]*text-transform: var\(--badge-transform\);/);
    expect(CSS).not.toMatch(/\[data-theme="[a-z]+"\]\s+\.[a-z]/);
  });
});

/// The code surface follows the user's terminal palette, not the page
/// appearance, so its fallbacks are exempt. So are two rules that draw
/// on top of terminal colours rather than page surfaces: the theme
/// preview's vignette and the hairline around an ANSI swatch.
const APPEARANCE_INDEPENDENT = [
  "var(--code-",
  "box-shadow: inset 0 0 28px rgba(0, 0, 0, 0.16);",
  "box-shadow: inset 0 0 0 1px rgba(255, 255, 255, 0.12);",
];

describe("colour literals outside the palette", () => {
  const body = `${CSS.slice(CSS.indexOf('[data-appearance="dark"]'))}\n${CONNECTION_CSS}`;

  it("leaves no rule painting a colour no appearance can reach", () => {
    const offenders = body.split("\n").filter((line) => {
      if (!/#[0-9a-fA-F]{3,8}\b|rgba?\(/.test(line)) return false;
      if (line.includes("color-mix(")) return false;
      return !APPEARANCE_INDEPENDENT.some((allowed) => line.includes(allowed));
    });
    expect(offenders.map((line) => line.trim())).toEqual([]);
  });

  it("keeps each --code-* fallback equal to the built-in palette's own value", () => {
    // The fallback is what paints the diff before a theme has loaded, so
    // a drifted one shows as a flash of the wrong colour.
    const expected = codeSurfaceVariables(BUILTIN_TERMINAL_THEME);
    const found = new Map(
      [...body.matchAll(/var\((--code-[a-z-]+), (#[0-9a-f]{6})\)/g)].map((m) => [m[1], m[2]]),
    );
    expect(found.size).toBeGreaterThan(10);
    for (const [name, value] of found) expect([name, value]).toEqual([name, expected[name]]);
  });
});

function luminance(color: string): number {
  const channels = [1, 3, 5].map((start) => Number.parseInt(color.slice(start, start + 2), 16) / 255);
  const linear = channels.map((c) => (c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4));
  return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
}
