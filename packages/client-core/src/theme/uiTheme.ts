export const USER_UI_THEME_KEY = "app.theme";

// "standard" is the stored value for Midnight, so accounts that chose it
// before the rename keep it.
export const UI_THEMES = ["standard", "graphite", "studio"] as const;

export type UiTheme = (typeof UI_THEMES)[number];

/// Stored values that are no longer offered, and the theme each one reads as.
const RETIRED_UI_THEMES: Readonly<Record<string, UiTheme>> = { compact: "graphite" };

const UI_THEME_LABELS: Readonly<Record<UiTheme, string>> = {
  standard: "Midnight",
  graphite: "Graphite",
  studio: "Studio",
};

export function uiThemeFromStoredValue(value: unknown): UiTheme | undefined {
  if (typeof value !== "string") return undefined;
  if (UI_THEMES.includes(value as UiTheme)) return value as UiTheme;
  return Object.hasOwn(RETIRED_UI_THEMES, value) ? RETIRED_UI_THEMES[value] : undefined;
}

export function parseUiTheme(stored: string | undefined): UiTheme {
  if (stored === undefined) return "standard";
  try {
    return uiThemeFromStoredValue(JSON.parse(stored)) ?? "standard";
  } catch {
    return "standard";
  }
}

export function uiThemeLabel(theme: UiTheme): string {
  return UI_THEME_LABELS[theme];
}
