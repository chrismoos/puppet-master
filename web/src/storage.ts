export const SELECTED_SESSION_KEY = "pm.selectedSession";
export const SHOW_ENDED_KEY = "pm.showEnded";
export const NOTIFY_ENABLED_KEY = "pm.notify.enabled";
export const NOTIFY_SOUND_KEY = "pm.notify.sound";
/// Set once notification permission has been auto-requested, so the
/// prompt is never shown on load more than once.
export const NOTIFY_ASKED_KEY = "pm.notify.asked";
export const COLLAPSED_BUCKETS_KEY = "pm.collapsed.buckets";
export const COLLAPSED_SUPERVISORS_KEY = "pm.collapsed.supervisors";
export const SUPERVISOR_FILTERS_KEY = "pm.supervisorFilters";
export const SIDEBAR_WIDTH_KEY = "pm.sidebarWidth";
export const REVIEW_RAIL_WIDTH_KEY = "pm.reviewRailWidth";
export const REVIEW_RAIL_COLLAPSED_KEY = "pm.reviewRailCollapsed";
const LEGACY_VIEW_TABS_KEY = "pm.viewTabs";
const LEGACY_COLLAPSED_PROJECTS_KEY = "pm.collapsed.projects";
const LEGACY_BOARD_SIDEBAR_PINNED_KEY = "pm.boardSidebarPinned";

/** Smallest and largest the sidebar may be dragged to, in pixels. */
export const SIDEBAR_MIN_WIDTH = 220;
export const SIDEBAR_MAX_WIDTH = 620;
export const SIDEBAR_DEFAULT_WIDTH = 304;

export function readSidebarWidth(): number {
  return readWidth(SIDEBAR_WIDTH_KEY, SIDEBAR_DEFAULT_WIDTH, SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
}

/** A stored width from a wider window, or from limits that have since
 * moved, still has to land inside today's. */
function readWidth(key: string, fallback: number, min: number, max: number): number {
  const raw = readString(key);
  const n = raw ? Number.parseInt(raw, 10) : NaN;
  if (Number.isNaN(n)) return fallback;
  return Math.min(max, Math.max(min, n));
}

export const CONNECTION_PANEL_WIDTH_KEY = "pm.connectionPanelWidth";
/// Setup panels the user closed, as session:connection:proposal keys, so a
/// draft stays closed across reloads until the agent brings new work.
export const CONNECTION_PANEL_DISMISSED_KEY = "pm.connectionPanelDismissed";
/** Smallest and largest the connection panel beside a session may be dragged to, in pixels. */
export const CONNECTION_PANEL_MIN_WIDTH = 360;
export const CONNECTION_PANEL_MAX_WIDTH = 960;
export const CONNECTION_PANEL_DEFAULT_WIDTH = 440;

export function readConnectionPanelWidth(): number {
  return readWidth(
    CONNECTION_PANEL_WIDTH_KEY,
    CONNECTION_PANEL_DEFAULT_WIDTH,
    CONNECTION_PANEL_MIN_WIDTH,
    CONNECTION_PANEL_MAX_WIDTH,
  );
}

/** Smallest and largest the review file list may be dragged to, in pixels. */
export const REVIEW_RAIL_MIN_WIDTH = 160;
export const REVIEW_RAIL_MAX_WIDTH = 620;
export const REVIEW_RAIL_DEFAULT_WIDTH = 240;

/** Width the collapsed file list keeps, in pixels: enough for the
 * control that brings it back and nothing else. Collapsing never sets
 * the stored width, so expanding restores whatever the reader dragged. */
export const REVIEW_RAIL_COLLAPSED_WIDTH = 26;

export function readReviewRailWidth(): number {
  return readWidth(REVIEW_RAIL_WIDTH_KEY, REVIEW_RAIL_DEFAULT_WIDTH, REVIEW_RAIL_MIN_WIDTH, REVIEW_RAIL_MAX_WIDTH);
}

export function readReviewRailCollapsed(): boolean {
  return readBool(REVIEW_RAIL_COLLAPSED_KEY, false);
}

export function readString(key: string): string | null {
  try {
    return localStorage.getItem(key);
  } catch {
    return null;
  }
}

export function writeString(key: string, value: string | null): void {
  try {
    if (value === null) localStorage.removeItem(key);
    else localStorage.setItem(key, value);
  } catch {
    // Storage may be unavailable (private mode, quota); preferences degrade to defaults.
  }
}

export function readSessionString(key: string): string | null {
  try {
    return sessionStorage.getItem(key);
  } catch {
    return null;
  }
}

export function writeSessionString(key: string, value: string | null): void {
  try {
    if (value === null) sessionStorage.removeItem(key);
    else sessionStorage.setItem(key, value);
  } catch {
    return;
  }
}

export function readBool(key: string, fallback: boolean): boolean {
  const raw = readString(key);
  if (raw === null) return fallback;
  return raw === "1";
}

export function writeBool(key: string, value: boolean): void {
  writeString(key, value ? "1" : "0");
}

export function readStringSet(key: string): Set<string> {
  const raw = readString(key);
  if (!raw) return new Set();
  try {
    const parsed = JSON.parse(raw) as unknown;
    return new Set(Array.isArray(parsed) ? parsed.map(String) : []);
  } catch {
    return new Set();
  }
}

export function writeStringSet(key: string, value: ReadonlySet<string>): void {
  writeString(key, JSON.stringify([...value]));
}

export function readStringMap(key: string): Map<string, string> {
  const raw = readString(key);
  if (!raw) return new Map();
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!Array.isArray(parsed)) return new Map();
    return new Map(parsed
      .filter((entry): entry is [unknown, unknown] => Array.isArray(entry) && entry.length === 2)
      .map(([entryKey, value]) => [String(entryKey), String(value)]));
  } catch {
    return new Map();
  }
}

export function writeStringMap(key: string, value: ReadonlyMap<string, string>): void {
  writeString(key, JSON.stringify([...value]));
}

export function removeLegacyNavigationPreferences(): void {
  writeString(LEGACY_VIEW_TABS_KEY, null);
  writeString(LEGACY_BOARD_SIDEBAR_PINNED_KEY, null);
  writeString(LEGACY_COLLAPSED_PROJECTS_KEY, null);
}
