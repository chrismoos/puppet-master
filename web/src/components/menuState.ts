/** Identifies the single open popover menu, or null when none is open. */
export type OpenMenu = string | null;

/** Toggles a menu key against the current selection, enforcing one open at a time. */
export function toggleMenu(current: OpenMenu, key: string): OpenMenu {
  return current === key ? null : key;
}
