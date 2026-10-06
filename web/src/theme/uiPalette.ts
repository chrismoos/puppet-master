import { contrastRatio } from "@puppet-master/client-core/theme/terminalTheme";
import type { UiTheme } from "@puppet-master/client-core/theme/uiTheme";

export type Appearance = "light" | "dark";
export type Palette = Record<string, string>;

const TOKEN_PAIR = /^\s*(--[a-z0-9-]+):\s*light-dark\(\s*(#[0-9a-f]{3,8})\s*,\s*(#[0-9a-f]{3,8})\s*\)\s*;/gim;

/// Both appearances of one application theme as read from the stylesheet
/// itself, so the contrast check runs against what ships rather than a
/// second copy of the values. The stored name selects the block: each
/// theme declares its pairs under `[data-theme="<name>"]`.
export function parseAppearancePalettes(css: string, theme: UiTheme = "standard"): Record<Appearance, Palette> {
  const light: Palette = {};
  const dark: Palette = {};
  for (const block of themeBlocks(css, theme)) {
    for (const match of block.matchAll(TOKEN_PAIR)) {
      light[match[1]] = match[2].toLowerCase();
      dark[match[1]] = match[3].toLowerCase();
    }
  }
  return { light, dark };
}

/// The declaration blocks whose selector list names the theme.
export function themeBlocks(css: string, theme: UiTheme): string[] {
  const selector = `[data-theme="${theme}"]`;
  const blocks: string[] = [];
  for (const match of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    const selectors = match[1].replace(/\/\*[\s\S]*?\*\//g, "").split(",").map((part) => part.trim());
    if (selectors.includes(selector)) blocks.push(match[2]);
  }
  return blocks;
}

const SURFACES = ["--bg", "--panel", "--panel-2", "--panel-3"];

/// Body text and every accent double as text on all four surfaces, so
/// they carry the 4.5:1 that small text needs. --dim is the deliberately
/// faint gutter tone and is held to the 3:1 of large or incidental text.
const TEXT_TOKENS: [string, number][] = [
  ["--text", 4.5],
  ["--muted", 4.5],
  ["--bright", 4.5],
  ["--amber", 4.5],
  ["--blue", 4.5],
  ["--green", 4.5],
  ["--red", 4.5],
  ["--slate", 4.5],
  ["--dim", 3],
];

const FILLED_ACCENTS = ["--amber", "--blue", "--green", "--red", "--slate"];

const STATUS_FAMILIES = ["warn", "ok", "danger", "info", "role", "supervisor"];

/// Separators are not text and are not held to a text ratio, but they do
/// have to be visible against the surface they divide.
const SEPARATORS: [string, string, number][] = [
  ["--line", "--panel", 1.25],
  ["--line", "--bg", 1.25],
  ["--line-soft", "--panel", 1.1],
  ["--line-strong", "--panel", 1.4],
];

export function uiContrastWarnings(palette: Palette): string[] {
  const warnings: string[] = [];
  const check = (foreground: string, background: string, floor: number, what: string) => {
    const a = palette[foreground];
    const b = palette[background];
    if (!a || !b) {
      warnings.push(`${what}: ${a ? background : foreground} is not defined as a light-dark() pair`);
      return;
    }
    const ratio = contrastRatio(a, b);
    if (ratio < floor) {
      warnings.push(`${what} is ${ratio.toFixed(2)}:1, below ${floor}:1`);
    }
  };

  for (const [token, floor] of TEXT_TOKENS) {
    for (const surface of SURFACES) check(token, surface, floor, `${token} on ${surface}`);
  }
  for (const accent of FILLED_ACCENTS) {
    check("--accent-ink", accent, 4.5, `--accent-ink on ${accent}`);
  }
  for (const family of STATUS_FAMILIES) {
    check(`--${family}-text`, `--${family}-bg`, 4.5, `--${family}-text on --${family}-bg`);
    check(`--${family}-line`, `--${family}-bg`, 1.5, `--${family}-line on --${family}-bg`);
  }
  for (const [line, surface, floor] of SEPARATORS) check(line, surface, floor, `${line} on ${surface}`);
  return warnings;
}
