// Semantic colour tokens for the mobile UI, aligned with the web app's
// CSS custom properties (web/src/styles.css) so both platforms share the
// same visual language. Prefer these over hardcoded hex values in screens.

import { SessionState } from "@puppet-master/client-core/gen/pm/v1/pm_pb";

// ---------------------------------------------------------------------------
// Palette – matches the web app's --var tokens
// ---------------------------------------------------------------------------

export const colors = {
  // Backgrounds
  bg: "#0b0e14",
  panel: "#10141c",
  panelAlt: "#161b26",
  surface: "#1a202b",

  // Text
  text: "#c9ceda",
  textBright: "#eef1f6",
  textMuted: "#6e7889",

  // Borders & dividers
  line: "#232a37",
  lineSoft: "#1a202b",

  // Semantic
  blue: "#4da3ff",
  amber: "#ffb224",
  green: "#3dd68c",
  red: "#ff5d5d",
  slate: "#9aa4b8",
} as const;

// ---------------------------------------------------------------------------
// Session state → colour mapping
// ---------------------------------------------------------------------------

export function stateColor(state: SessionState): string {
  switch (state) {
    case SessionState.NEEDS_INPUT:
      return colors.amber;
    case SessionState.FAILED:
      return colors.red;
    case SessionState.WORKING:
      return colors.blue;
    case SessionState.IDLE:
      return colors.green;
    case SessionState.STARTING:
    case SessionState.AWAITING_WORKER:
      return colors.slate;
    case SessionState.EXITED:
    case SessionState.UNSPECIFIED:
      return colors.textMuted;
  }
}

export function stateBackgroundColor(state: SessionState): string | null {
  switch (state) {
    case SessionState.NEEDS_INPUT:
      return "rgba(255, 178, 36, 0.08)";
    case SessionState.FAILED:
      return "rgba(255, 93, 93, 0.08)";
    default:
      return null;
  }
}
