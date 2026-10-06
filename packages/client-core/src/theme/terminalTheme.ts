export const USER_TERMINAL_THEME_KEY = "terminal.theme";
export const TERMINAL_THEME_KIND = "puppet-master-terminal-theme";
export const TERMINAL_THEME_VERSION = 1;
export const TERMINAL_THEME_MAX_BYTES = 64 * 1024;

const NAME_MAX_CHARS = 80;
const METADATA_MAX_CHARS = 160;

export const ANSI_COLOR_KEYS = [
  "black", "red", "green", "yellow", "blue", "magenta", "cyan", "white",
  "brightBlack", "brightRed", "brightGreen", "brightYellow",
  "brightBlue", "brightMagenta", "brightCyan", "brightWhite",
] as const;

export const CORE_COLOR_KEYS = [
  "foreground", "background", "cursor", "cursorAccent",
  "selectionForeground", "selectionBackground", ...ANSI_COLOR_KEYS,
] as const;

const OPTIONAL_COLOR_KEYS = [
  "selectionInactiveBackground",
  "scrollbarSliderBackground",
  "scrollbarSliderHoverBackground",
  "scrollbarSliderActiveBackground",
  "overviewRulerBorder",
] as const;

type CoreColorKey = (typeof CORE_COLOR_KEYS)[number];
type OptionalColorKey = (typeof OPTIONAL_COLOR_KEYS)[number];

/** Structurally compatible with xterm.js ITheme without importing it. */
export type XtermTheme = Partial<Record<CoreColorKey | OptionalColorKey, string>> & {
  extendedAnsi?: string[];
};

export type TerminalThemeColors = Record<CoreColorKey, string> &
  Partial<Record<OptionalColorKey, string>> & {
    extendedAnsi?: string[];
  };

export interface TerminalTheme {
  kind: typeof TERMINAL_THEME_KIND;
  version: typeof TERMINAL_THEME_VERSION;
  name: string;
  author?: string;
  license?: string;
  colors: TerminalThemeColors;
}

export interface ThemeImportResult {
  theme: TerminalTheme;
  warnings: string[];
  format: "native" | "ghostty";
}

export class ThemeParseError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "ThemeParseError";
  }
}

export const BUILTIN_TERMINAL_THEME: TerminalTheme = {
  kind: TERMINAL_THEME_KIND,
  version: TERMINAL_THEME_VERSION,
  name: "Puppet Master",
  colors: {
    foreground: "#c9ceda",
    background: "#0b0e14",
    cursor: "#ffb224",
    cursorAccent: "#0b0e14",
    selectionForeground: "#eef1f6",
    selectionBackground: "#2b3548",
    black: "#2e3436",
    red: "#cc0000",
    green: "#4e9a06",
    yellow: "#c4a000",
    blue: "#3465a4",
    magenta: "#75507b",
    cyan: "#06989a",
    white: "#d3d7cf",
    brightBlack: "#555753",
    brightRed: "#ef2929",
    brightGreen: "#8ae234",
    brightYellow: "#fce94f",
    brightBlue: "#729fcf",
    brightMagenta: "#ad7fa8",
    brightCyan: "#34e2e2",
    brightWhite: "#eeeeec",
  },
};

export function parseNativeTerminalTheme(input: string): ThemeImportResult {
  enforceImportSize(input);
  let raw: unknown;
  try {
    raw = JSON.parse(input);
  } catch (error) {
    throw new ThemeParseError(`Malformed JSON: ${error instanceof Error ? error.message : String(error)}`);
  }
  const theme = validateTerminalTheme(raw);
  return { theme, warnings: contrastWarnings(theme), format: "native" };
}

export function validateTerminalTheme(raw: unknown): TerminalTheme {
  const theme = record(raw, "theme");
  rejectUnknown(theme, ["kind", "version", "name", "author", "license", "colors"], "theme");
  if (theme.kind !== TERMINAL_THEME_KIND) {
    throw new ThemeParseError(`Unsupported theme kind ${JSON.stringify(theme.kind)}`);
  }
  if (theme.version !== TERMINAL_THEME_VERSION) {
    throw new ThemeParseError(`Unsupported terminal theme version ${JSON.stringify(theme.version)}`);
  }
  const name = boundedString(theme.name, "name", NAME_MAX_CHARS, false);
  const author = optionalBoundedString(theme.author, "author", METADATA_MAX_CHARS);
  const license = optionalBoundedString(theme.license, "license", METADATA_MAX_CHARS);
  const rawColors = record(theme.colors, "colors");
  rejectUnknown(rawColors, [...CORE_COLOR_KEYS, ...OPTIONAL_COLOR_KEYS, "extendedAnsi"], "colors");
  const colors = {} as TerminalThemeColors;
  for (const key of CORE_COLOR_KEYS) {
    colors[key] = normalizeColor(rawColors[key], key, key === "selectionBackground");
  }
  for (const key of OPTIONAL_COLOR_KEYS) {
    if (rawColors[key] !== undefined) {
      colors[key] = normalizeColor(
        rawColors[key],
        key,
        key === "selectionInactiveBackground",
      );
    }
  }
  if (rawColors.extendedAnsi !== undefined) {
    if (!Array.isArray(rawColors.extendedAnsi) || rawColors.extendedAnsi.length !== 240) {
      const length = Array.isArray(rawColors.extendedAnsi) ? rawColors.extendedAnsi.length : "non-array";
      throw new ThemeParseError(`extendedAnsi must contain all 240 colors (ANSI 16–255), not ${length}`);
    }
    colors.extendedAnsi = rawColors.extendedAnsi.map((value, index) =>
      normalizeColor(value, `extendedAnsi[${index + 16}]`, false));
  }
  return {
    kind: TERMINAL_THEME_KIND,
    version: TERMINAL_THEME_VERSION,
    name,
    ...(author === undefined ? {} : { author }),
    ...(license === undefined ? {} : { license }),
    colors,
  };
}

export function parseGhosttyTerminalTheme(input: string, sourceName = "Imported Ghostty"): ThemeImportResult {
  enforceImportSize(input);
  const warnings: string[] = [];
  const values = new Map<CoreColorKey, string>();
  const palette = new Map<number, string>();
  const seen = new Map<string, number>();
  const simple = new Map<string, CoreColorKey>([
    ["foreground", "foreground"],
    ["background", "background"],
    ["cursor-color", "cursor"],
    ["cursor-text", "cursorAccent"],
    ["selection-foreground", "selectionForeground"],
    ["selection-background", "selectionBackground"],
  ]);

  input.split(/\r?\n/).forEach((rawLine, offset) => {
    const lineNumber = offset + 1;
    const line = rawLine.trim();
    if (!line || line.startsWith("#")) return;
    const equals = line.indexOf("=");
    if (equals < 0) {
      warnings.push(`Line ${lineNumber}: ignored malformed Ghostty setting`);
      return;
    }
    const key = line.slice(0, equals).trim().toLowerCase();
    const value = line.slice(equals + 1).trim();
    const mapped = simple.get(key);
    if (mapped) {
      diagnoseDuplicate(seen, key, lineNumber, warnings);
      values.set(mapped, normalizeColor(value, `Ghostty ${key} on line ${lineNumber}`, mapped === "selectionBackground"));
      return;
    }
    if (key === "palette") {
      const paletteEquals = value.indexOf("=");
      if (paletteEquals < 0) {
        throw new ThemeParseError(`Line ${lineNumber}: palette must be INDEX=#RRGGBB`);
      }
      const indexText = value.slice(0, paletteEquals).trim();
      const index = Number(indexText);
      if (!/^\d{1,3}$/.test(indexText) || !Number.isInteger(index) || index < 0 || index > 255) {
        throw new ThemeParseError(`Line ${lineNumber}: invalid Ghostty palette index ${JSON.stringify(indexText)}`);
      }
      const paletteKey = `palette:${index}`;
      diagnoseDuplicate(seen, paletteKey, lineNumber, warnings);
      palette.set(index, normalizeColor(value.slice(paletteEquals + 1).trim(), `Ghostty palette ${index} on line ${lineNumber}`, false));
      return;
    }
    // Includes, paths, commands, and every other non-color setting are data
    // only: report and ignore them, never resolve or evaluate them.
    warnings.push(`Line ${lineNumber}: ignored Ghostty key ${JSON.stringify(key || "(empty)")}`);
  });

  ANSI_COLOR_KEYS.forEach((key, index) => {
    if (palette.has(index)) values.set(key, palette.get(index)!);
  });
  const missing: string[] = [];
  for (const key of CORE_COLOR_KEYS) {
    if (values.has(key)) continue;
    values.set(key, BUILTIN_TERMINAL_THEME.colors[key]);
    missing.push(key);
  }
  if (missing.length > 0) {
    warnings.push(`Filled missing colors from Puppet Master: ${missing.join(", ")}`);
  }

  const extendedEntries = [...palette.keys()].filter((index) => index >= 16);
  let extendedAnsi: string[] | undefined;
  if (extendedEntries.length === 240) {
    extendedAnsi = Array.from({ length: 240 }, (_, offset) => palette.get(offset + 16)!);
  } else if (extendedEntries.length > 0) {
    warnings.push(`Ignored partial extended palette (${extendedEntries.length}/240 colors); ANSI 16–255 must be complete`);
  }
  const colors = Object.fromEntries(values) as unknown as TerminalThemeColors;
  if (extendedAnsi) colors.extendedAnsi = extendedAnsi;
  const theme: TerminalTheme = {
    kind: TERMINAL_THEME_KIND,
    version: TERMINAL_THEME_VERSION,
    name: [...sourceName.trim()].slice(0, NAME_MAX_CHARS).join("") || "Imported Ghostty",
    colors,
  };
  warnings.push(...contrastWarnings(theme));
  return { theme, warnings, format: "ghostty" };
}

export function terminalThemeToXterm(theme: TerminalTheme): XtermTheme {
  const { extendedAnsi, ...colors } = theme.colors;
  return {
    ...colors,
    ...(extendedAnsi ? { extendedAnsi: [...extendedAnsi] } : {}),
  };
}

export function exportNativeTerminalTheme(theme: TerminalTheme): string {
  return `${JSON.stringify(validateTerminalTheme(theme), null, 2)}\n`;
}

export function contrastWarnings(theme: TerminalTheme): string[] {
  const ratio = contrastRatio(theme.colors.foreground, theme.colors.background);
  return ratio < 4.5
    ? [`Foreground/background contrast is ${ratio.toFixed(2)}:1; 4.5:1 is recommended for text.`]
    : [];
}

/// Whether a palette expects a light or a dark background. The theme
/// picker labels entries with it, and anything colouring page content
/// from the palette needs it to know which ANSI intensity will read.
export function terminalThemeAppearance(theme: TerminalTheme): "light" | "dark" {
  return relativeLuminance(theme.colors.background) > 0.5 ? "light" : "dark";
}

export function contrastRatio(foreground: string, background: string): number {
  const a = relativeLuminance(foreground);
  const b = relativeLuminance(background);
  return (Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05);
}

function relativeLuminance(color: string): number {
  const channels = [1, 3, 5].map((start) => Number.parseInt(color.slice(start, start + 2), 16) / 255);
  const linear = channels.map((channel) => channel <= 0.04045 ? channel / 12.92 : ((channel + 0.055) / 1.055) ** 2.4);
  return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
}

function diagnoseDuplicate(
  seen: Map<string, number>,
  key: string,
  line: number,
  warnings: string[],
): void {
  const previous = seen.get(key);
  if (previous !== undefined) {
    warnings.push(`Line ${line}: duplicate ${key} replaces line ${previous}`);
  }
  seen.set(key, line);
}

function enforceImportSize(input: string): void {
  if (new TextEncoder().encode(input).byteLength > TERMINAL_THEME_MAX_BYTES) {
    throw new ThemeParseError("Theme exceeds the 64 KiB import limit");
  }
}

function normalizeColor(value: unknown, field: string, alpha: boolean): string {
  if (typeof value !== "string") {
    throw new ThemeParseError(`Color ${field} must be a string`);
  }
  const pattern = alpha ? /^#[0-9a-fA-F]{6}(?:[0-9a-fA-F]{2})?$/ : /^#[0-9a-fA-F]{6}$/;
  if (!pattern.test(value)) {
    throw new ThemeParseError(`Color ${field} must be ${alpha ? "#RRGGBB or #RRGGBBAA" : "#RRGGBB"}, not ${JSON.stringify(value)}`);
  }
  return value.toLowerCase();
}

function record(value: unknown, field: string): Record<string, unknown> {
  if (!value || typeof value !== "object" || Array.isArray(value)) {
    throw new ThemeParseError(`${field} must be an object`);
  }
  return value as Record<string, unknown>;
}

function rejectUnknown(value: Record<string, unknown>, known: readonly string[], field: string): void {
  const unknown = Object.keys(value).filter((key) => !known.includes(key));
  if (unknown.length > 0) {
    throw new ThemeParseError(`Unknown ${field} field${unknown.length === 1 ? "" : "s"}: ${unknown.join(", ")}`);
  }
}

function boundedString(value: unknown, field: string, max: number, empty: boolean): string {
  if (typeof value !== "string" || (!empty && value.trim().length === 0) || [...value].length > max) {
    throw new ThemeParseError(`${field} must contain ${empty ? `at most ${max}` : `1–${max}`} characters`);
  }
  return value;
}

function optionalBoundedString(value: unknown, field: string, max: number): string | undefined {
  return value === undefined ? undefined : boundedString(value, field, max, true);
}
