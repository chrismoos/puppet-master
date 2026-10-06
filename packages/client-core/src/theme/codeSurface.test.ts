import { describe, expect, it } from "vitest";
import { BUILTIN_TERMINAL_THEMES } from "./builtinTerminalThemes";
import { CODE_TOKEN_ROLES, codeSurface, codeSurfaceVariables } from "./codeSurface";
import {
  BUILTIN_TERMINAL_THEME,
  contrastRatio,
  type TerminalTheme,
} from "./terminalTheme";

const flat = (background: string, foreground: string): TerminalTheme => ({
  ...BUILTIN_TERMINAL_THEME,
  name: "Flat",
  colors: Object.fromEntries(
    Object.keys(BUILTIN_TERMINAL_THEME.colors).map((key) => [
      key,
      key === "background" ? background : key === "foreground" ? foreground : background,
    ]),
  ) as unknown as TerminalTheme["colors"],
});

describe("the review diff's code surface", () => {
  it("keeps the terminal palette's own background and foreground", () => {
    for (const theme of BUILTIN_TERMINAL_THEMES) {
      const surface = codeSurface(theme);
      expect(surface.background).toBe(theme.colors.background);
      expect(surface.foreground).toBe(theme.colors.foreground);
    }
  });

  it("keeps every token legible on the background and on both diff washes", () => {
    for (const theme of BUILTIN_TERMINAL_THEMES) {
      const surface = codeSurface(theme);
      for (const role of CODE_TOKEN_ROLES) {
        for (const behind of [surface.background, surface.addBackground, surface.delBackground]) {
          expect(
            contrastRatio(surface.tokens[role], behind),
            `${theme.name} ${role} on ${behind}`,
          ).toBeGreaterThanOrEqual(3);
        }
      }
      for (const dim of [surface.gutter, surface.hunk]) {
        expect(contrastRatio(dim, surface.background), theme.name).toBeGreaterThanOrEqual(3);
      }
    }
  });

  it("leaves a wash a reader can see without swamping the text on it", () => {
    for (const theme of BUILTIN_TERMINAL_THEMES) {
      const surface = codeSurface(theme);
      const ceiling = Math.min(4.5, contrastRatio(theme.colors.foreground, theme.colors.background) - 0.35);
      for (const [wash, hover] of [
        [surface.addBackground, surface.addHoverBackground],
        [surface.delBackground, surface.delHoverBackground],
      ]) {
        // A red tint on a near-black background barely shifts luminance
        // while being plainly visible, so a wash's presence is measured
        // as a channel distance rather than as a contrast ratio.
        expect(channelDistance(wash, surface.background), theme.name).toBeGreaterThanOrEqual(6);
        expect(contrastRatio(surface.foreground, wash), theme.name).toBeGreaterThanOrEqual(ceiling);
        expect(channelDistance(hover, surface.background), theme.name)
          .toBeGreaterThan(channelDistance(wash, surface.background));
      }
      expect(surface.addBackground).not.toBe(surface.delBackground);
    }
  });

  it("abandons an ANSI slot that is the palette's own background", () => {
    const solarizedDark = BUILTIN_TERMINAL_THEMES.find((theme) => theme.name === "Solarized Dark")!;
    // Solarized's ANSI 8 is base03, which is exactly this palette's
    // background, so a fixed bright-half mapping renders comments at 1:1.
    expect(solarizedDark.colors.brightBlack).toBe(solarizedDark.colors.background);
    const comment = codeSurface(solarizedDark).tokens.comment;
    expect(comment).not.toBe(solarizedDark.colors.brightBlack);
    expect(contrastRatio(comment, solarizedDark.colors.background)).toBeGreaterThanOrEqual(3);
    // Lifted only as far as it had to be: a comment that lands on the
    // body foreground has stopped reading as a comment.
    expect(comment).not.toBe(solarizedDark.colors.foreground);
    expect(contrastRatio(comment, solarizedDark.colors.background))
      .toBeLessThan(contrastRatio(solarizedDark.colors.foreground, solarizedDark.colors.background));
  });

  it("prefers a palette's own colour to a blend when that colour reads", () => {
    const dracula = BUILTIN_TERMINAL_THEMES.find((theme) => theme.name === "Dracula")!;
    const tokens = codeSurface(dracula).tokens;
    expect(tokens.string).toBe(dracula.colors.brightGreen);
    expect(tokens.keyword).toBe(dracula.colors.brightMagenta);
  });

  it("still produces a legible surface from a palette with no usable colours", () => {
    for (const [background, foreground] of [["#101010", "#e8e8e8"], ["#fbfbfb", "#1a1a1a"]]) {
      const surface = codeSurface(flat(background, foreground));
      for (const role of CODE_TOKEN_ROLES) {
        expect(
          contrastRatio(surface.tokens[role], background),
          `${background} ${role}`,
        ).toBeGreaterThanOrEqual(3);
      }
    }
  });

  it("publishes one custom property per colour the stylesheet reads", () => {
    const variables = codeSurfaceVariables(BUILTIN_TERMINAL_THEME);
    expect(Object.keys(variables)).toEqual([
      "--code-bg", "--code-fg", "--code-gutter", "--code-hunk", "--code-line",
      "--code-row-hover", "--code-add-bg", "--code-add-bg-hover",
      "--code-del-bg", "--code-del-bg-hover",
      ...CODE_TOKEN_ROLES.map((role) => `--code-${role}`),
    ]);
    for (const value of Object.values(variables)) expect(value).toMatch(/^#[0-9a-f]{6}$/);
  });
});

function channelDistance(a: string, b: string): number {
  return Math.max(...[1, 3, 5].map((start) =>
    Math.abs(Number.parseInt(a.slice(start, start + 2), 16) - Number.parseInt(b.slice(start, start + 2), 16))));
}
