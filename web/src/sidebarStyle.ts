/** Synchronized user-settings key holding how the sidebar switches buckets. */
export const USER_SIDEBAR_STYLE_KEY = "app.sidebar_style";

export const SIDEBAR_STYLES = ["dropdown", "rail"] as const;

export type SidebarStyle = (typeof SIDEBAR_STYLES)[number];

/** The stored choice, or the dropdown when nothing valid is stored. */
export function parseSidebarStyle(stored: string | undefined): SidebarStyle {
  if (stored === undefined) return "dropdown";
  try {
    const value = JSON.parse(stored) as unknown;
    return SIDEBAR_STYLES.includes(value as SidebarStyle) ? value as SidebarStyle : "dropdown";
  } catch {
    return "dropdown";
  }
}
