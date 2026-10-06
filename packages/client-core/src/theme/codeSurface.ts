import { contrastRatio, type TerminalTheme } from "./terminalTheme";

export const CODE_TOKEN_ROLES = [
  "comment", "keyword", "string", "number", "title", "type", "meta",
] as const;

export type CodeTokenRole = (typeof CODE_TOKEN_ROLES)[number];

export interface CodeSurface {
  background: string;
  foreground: string;
  gutter: string;
  hunk: string;
  line: string;
  rowHover: string;
  addBackground: string;
  addHoverBackground: string;
  delBackground: string;
  delHoverBackground: string;
  tokens: Record<CodeTokenRole, string>;
}

/// Each role's preferred ANSI slot and the fallback of the pair. Terminal
/// palettes disagree about which intensity carries a colour: some put the
/// vivid tone in the bright half, and Solarized puts its grey base tones
/// there, so neither half can be assumed to be the legible one.
const ROLE_SLOTS: Record<CodeTokenRole, [keyof TerminalTheme["colors"], keyof TerminalTheme["colors"]]> = {
  comment: ["brightBlack", "black"],
  keyword: ["brightMagenta", "magenta"],
  string: ["brightGreen", "green"],
  number: ["brightCyan", "cyan"],
  title: ["brightBlue", "blue"],
  type: ["brightYellow", "yellow"],
  meta: ["brightRed", "red"],
};

/// Tokens are colour on top of colour and are held to the 3:1 of large or
/// decorated text rather than the 4.5:1 the body of the diff carries.
const TOKEN_FLOOR = 3;

const WASH_MAX = 0.16;
const WASH_MIN = 0.04;
const WASH_ALLOWANCE = 0.35;
const WASH_HOVER_CAP = 0.32;

/// A wash costs the text on it some contrast, so it is only ever as
/// strong as the palette can afford. A palette with room keeps the 4.5:1
/// small-text ratio under it; one already near that floor, as Solarized
/// is, gives up no more than WASH_ALLOWANCE of what it started with.
function washCeiling(foreground: string, background: string): number {
  return Math.min(4.5, contrastRatio(foreground, background) - WASH_ALLOWANCE);
}

/// The colours the review diff paints itself with, derived from whichever
/// terminal palette the user chose.
///
/// The diff keeps that palette's own background rather than the page's,
/// because the terminal beside it shows the same kind of content from the
/// same palette and a code surface that changed with the page appearance
/// would disagree with the terminal on every screen. Keeping the
/// background is not enough on its own: a fixed choice of the bright ANSI
/// half leaves Solarized's comments on their own background at 1:1, so
/// each role takes whichever half of its pair reads, and is blended
/// toward the foreground when neither does.
export function codeSurface(theme: TerminalTheme): CodeSurface {
  const background = theme.colors.background;
  const foreground = theme.colors.foreground;
  const ceiling = washCeiling(foreground, background);
  const addWeight = washWeight(theme.colors.green, background, foreground, ceiling);
  const delWeight = washWeight(theme.colors.red, background, foreground, ceiling);
  const addBackground = mix(theme.colors.green, background, addWeight);
  const delBackground = mix(theme.colors.red, background, delWeight);
  const addHoverBackground = mix(theme.colors.green, background, Math.min(addWeight * 2, WASH_HOVER_CAP));
  const delHoverBackground = mix(theme.colors.red, background, Math.min(delWeight * 2, WASH_HOVER_CAP));

  // A row's text sits on a wash as often as on the background, so a token
  // has to clear the floor against all three.
  const surfaces = [background, addBackground, delBackground];
  const tokens = {} as Record<CodeTokenRole, string>;
  for (const role of CODE_TOKEN_ROLES) {
    const [preferred, fallback] = ROLE_SLOTS[role];
    tokens[role] = resolve(
      [theme.colors[preferred] as string, theme.colors[fallback] as string],
      surfaces,
      foreground,
      TOKEN_FLOOR,
    );
  }

  return {
    background,
    foreground,
    gutter: resolve([mix(foreground, background, 0.5)], surfaces, foreground, TOKEN_FLOOR),
    hunk: resolve([mix(foreground, background, 0.65)], surfaces, foreground, TOKEN_FLOOR),
    line: mix(foreground, background, 0.2),
    rowHover: mix(foreground, background, 0.07),
    addBackground,
    addHoverBackground,
    delBackground,
    delHoverBackground,
    tokens,
  };
}

/// The published palette as CSS custom property names and values.
export function codeSurfaceVariables(theme: TerminalTheme): Record<string, string> {
  const surface = codeSurface(theme);
  const variables: Record<string, string> = {
    "--code-bg": surface.background,
    "--code-fg": surface.foreground,
    "--code-gutter": surface.gutter,
    "--code-hunk": surface.hunk,
    "--code-line": surface.line,
    "--code-row-hover": surface.rowHover,
    "--code-add-bg": surface.addBackground,
    "--code-add-bg-hover": surface.addHoverBackground,
    "--code-del-bg": surface.delBackground,
    "--code-del-bg-hover": surface.delHoverBackground,
  };
  for (const role of CODE_TOKEN_ROLES) variables[`--code-${role}`] = surface.tokens[role];
  return variables;
}

/// The strongest wash of `hue` the text can still be read on.
function washWeight(hue: string, background: string, foreground: string, ceiling: number): number {
  for (let step = Math.round(WASH_MAX * 100); step > Math.round(WASH_MIN * 100); step -= 1) {
    if (contrastRatio(foreground, mix(hue, background, step / 100)) >= ceiling) return step / 100;
  }
  return WASH_MIN;
}

/// The first candidate that clears `floor` against every surface, or the
/// best of them blended toward `anchor` until it does.
function resolve(candidates: string[], surfaces: string[], anchor: string, floor: number): string {
  const worst = (color: string) => Math.min(...surfaces.map((surface) => contrastRatio(color, surface)));
  for (const candidate of candidates) {
    if (worst(candidate) >= floor) return candidate;
  }
  const best = candidates.reduce((a, b) => (worst(a) >= worst(b) ? a : b));
  for (let step = 1; step <= 20; step += 1) {
    const blended = mix(anchor, best, step / 20);
    if (worst(blended) >= floor) return blended;
  }
  return anchor;
}

function mix(color: string, base: string, weight: number): string {
  const channels = [1, 3, 5].map((start) => {
    const a = Number.parseInt(color.slice(start, start + 2), 16);
    const b = Number.parseInt(base.slice(start, start + 2), 16);
    return Math.round(a * weight + b * (1 - weight));
  });
  return `#${channels.map((value) => value.toString(16).padStart(2, "0")).join("")}`;
}
