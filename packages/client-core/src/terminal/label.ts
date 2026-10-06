const TERMINAL_TAB_TITLE_CHARS = 32;

/** Produces a display label for a shell terminal tab. Prefers the live
 *  terminal title (e.g. the running command) over the configured one,
 *  falls back to "shell {id}" when neither is meaningful, and truncates
 *  long titles to TERMINAL_TAB_TITLE_CHARS. */
export function shellTabLabel(id: bigint, configuredTitle: string, liveTitle?: string): string {
  const configured = configuredTitle.trim();
  const title = liveTitle?.trim() || (configured.toLowerCase() === "shell" ? `shell ${id}` : configured) || `shell ${id}`;
  const chars = [...title];
  return chars.length > TERMINAL_TAB_TITLE_CHARS
    ? `${chars.slice(0, TERMINAL_TAB_TITLE_CHARS - 1).join("")}…`
    : title;
}
