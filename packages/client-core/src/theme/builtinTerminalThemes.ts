import {
  BUILTIN_TERMINAL_THEME,
  CORE_COLOR_KEYS,
  TERMINAL_THEME_KIND,
  TERMINAL_THEME_VERSION,
  type TerminalTheme,
} from "./terminalTheme";

/// Palettes taken from each project's own published terminal definition.
/// The upstream author and licence travel in every theme's metadata
/// because these are MIT-licensed works whose notice has to stay with
/// the colours; THIRD-PARTY-NOTICES.md repeats the same attribution.

const DRACULA: TerminalTheme = {
  kind: TERMINAL_THEME_KIND,
  version: TERMINAL_THEME_VERSION,
  name: "Dracula",
  author: "Dracula Theme",
  license: "MIT License, Copyright (c) 2016 Dracula Theme",
  colors: {
    foreground: "#f8f8f2",
    background: "#282a36",
    cursor: "#f8f8f2",
    cursorAccent: "#282a36",
    selectionForeground: "#f8f8f2",
    selectionBackground: "#44475a",
    black: "#21222c",
    red: "#ff5555",
    green: "#50fa7b",
    yellow: "#f1fa8c",
    blue: "#bd93f9",
    magenta: "#ff79c6",
    cyan: "#8be9fd",
    white: "#f8f8f2",
    brightBlack: "#6272a4",
    brightRed: "#ff6e6e",
    brightGreen: "#69ff94",
    brightYellow: "#ffffa5",
    brightBlue: "#d6acff",
    brightMagenta: "#ff92df",
    brightCyan: "#a4ffff",
    brightWhite: "#ffffff",
  },
};

const NORD: TerminalTheme = {
  kind: TERMINAL_THEME_KIND,
  version: TERMINAL_THEME_VERSION,
  name: "Nord",
  author: "Arctic Ice Studio",
  license: "MIT License, Copyright (c) 2016-present Arctic Ice Studio and Sven Greb",
  colors: {
    foreground: "#d8dee9",
    background: "#2e3440",
    cursor: "#d8dee9",
    cursorAccent: "#2e3440",
    selectionForeground: "#d8dee9",
    selectionBackground: "#434c5e",
    black: "#3b4252",
    red: "#bf616a",
    green: "#a3be8c",
    yellow: "#ebcb8b",
    blue: "#81a1c1",
    magenta: "#b48ead",
    cyan: "#88c0d0",
    white: "#e5e9f0",
    brightBlack: "#4c566a",
    brightRed: "#bf616a",
    brightGreen: "#a3be8c",
    brightYellow: "#ebcb8b",
    brightBlue: "#81a1c1",
    brightMagenta: "#b48ead",
    brightCyan: "#8fbcbb",
    brightWhite: "#eceff4",
  },
};

const GRUVBOX_DARK: TerminalTheme = {
  kind: TERMINAL_THEME_KIND,
  version: TERMINAL_THEME_VERSION,
  name: "Gruvbox Dark",
  author: "Pavel Pertsev",
  license: "MIT License, Copyright (c) 2018 Pavel Pertsev",
  colors: {
    foreground: "#ebdbb2",
    background: "#282828",
    cursor: "#ebdbb2",
    cursorAccent: "#282828",
    selectionForeground: "#ebdbb2",
    selectionBackground: "#504945",
    black: "#282828",
    red: "#cc241d",
    green: "#98971a",
    yellow: "#d79921",
    blue: "#458588",
    magenta: "#b16286",
    cyan: "#689d6a",
    white: "#a89984",
    brightBlack: "#928374",
    brightRed: "#fb4934",
    brightGreen: "#b8bb26",
    brightYellow: "#fabd2f",
    brightBlue: "#83a598",
    brightMagenta: "#d3869b",
    brightCyan: "#8ec07c",
    brightWhite: "#ebdbb2",
  },
};

/// Solarized fixes one sixteen-colour palette and swaps only the base
/// tones between its dark and light modes, so both themes below carry
/// identical ANSI values.
const SOLARIZED_ANSI = {
  black: "#073642",
  red: "#dc322f",
  green: "#859900",
  yellow: "#b58900",
  blue: "#268bd2",
  magenta: "#d33682",
  cyan: "#2aa198",
  white: "#eee8d5",
  brightBlack: "#002b36",
  brightRed: "#cb4b16",
  brightGreen: "#586e75",
  brightYellow: "#657b83",
  brightBlue: "#839496",
  brightMagenta: "#6c71c4",
  brightCyan: "#93a1a1",
  brightWhite: "#fdf6e3",
} as const;

const SOLARIZED_DARK: TerminalTheme = {
  kind: TERMINAL_THEME_KIND,
  version: TERMINAL_THEME_VERSION,
  name: "Solarized Dark",
  author: "Ethan Schoonover",
  license: "MIT License, Copyright (c) 2011 Ethan Schoonover",
  colors: {
    foreground: "#839496",
    background: "#002b36",
    cursor: "#839496",
    cursorAccent: "#002b36",
    selectionForeground: "#93a1a1",
    selectionBackground: "#073642",
    ...SOLARIZED_ANSI,
  },
};

/// Solarized names base00 body text and base01 emphasised text on a light
/// background. base00 (#657b83) on base3 reaches only 4.13:1, which the
/// contrast check bundled themes are held to rejects, so the emphasised
/// tone is the foreground here at 4.99:1.
const SOLARIZED_LIGHT: TerminalTheme = {
  kind: TERMINAL_THEME_KIND,
  version: TERMINAL_THEME_VERSION,
  name: "Solarized Light",
  author: "Ethan Schoonover",
  license: "MIT License, Copyright (c) 2011 Ethan Schoonover",
  colors: {
    foreground: "#586e75",
    background: "#fdf6e3",
    cursor: "#586e75",
    cursorAccent: "#fdf6e3",
    selectionForeground: "#586e75",
    selectionBackground: "#eee8d5",
    ...SOLARIZED_ANSI,
  },
};

/// The shipped palettes, in the order the picker lists them. The Puppet
/// Master default leads because selecting it is how a user gets back to
/// having no theme of their own.
export const BUILTIN_TERMINAL_THEMES: readonly TerminalTheme[] = [
  BUILTIN_TERMINAL_THEME,
  DRACULA,
  GRUVBOX_DARK,
  NORD,
  SOLARIZED_DARK,
  SOLARIZED_LIGHT,
];

export function findBuiltinTerminalTheme(name: string): TerminalTheme | undefined {
  return BUILTIN_TERMINAL_THEMES.find((theme) => theme.name === name);
}

/// The built-in a theme's colours are, or undefined for an imported one.
/// Identity is the palette rather than the name so that an import called
/// "Dracula" that differs from the bundled one stays its own entry.
export function builtinTerminalThemeMatching(theme: TerminalTheme): TerminalTheme | undefined {
  return BUILTIN_TERMINAL_THEMES.find((builtin) => sameTerminalPalette(builtin, theme));
}

export function sameTerminalPalette(a: TerminalTheme, b: TerminalTheme): boolean {
  for (const key of CORE_COLOR_KEYS) {
    if (a.colors[key] !== b.colors[key]) return false;
  }
  return (a.colors.extendedAnsi ?? []).join(",") === (b.colors.extendedAnsi ?? []).join(",");
}
