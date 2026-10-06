export const USER_APPEARANCE_KEY = "app.appearance";

/// The two appearances the page can be pinned to. "system" is not one of
/// them: following the operating system is the absence of a choice, so it
/// is stored as no setting at all rather than as a third value that would
/// have to be kept in step with the media query.
export const APPEARANCES = ["light", "dark"] as const;

export type Appearance = (typeof APPEARANCES)[number];

/// What the user has picked, or null while they are following the system.
export function parseAppearance(stored: string | undefined): Appearance | null {
  if (stored === undefined) return null;
  try {
    const value: unknown = JSON.parse(stored);
    return APPEARANCES.includes(value as Appearance) ? (value as Appearance) : null;
  } catch {
    return null;
  }
}

/// What choosing again should select, cycling system, light, dark.
export function nextAppearance(current: Appearance | null): Appearance | null {
  if (current === null) return "light";
  return current === "light" ? "dark" : null;
}

export function appearanceLabel(current: Appearance | null): string {
  return current ?? "system";
}
