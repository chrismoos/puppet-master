import type { ConnPhase } from "@puppet-master/client-core/state/reducer";

import type { DeviceAuthStatus } from "./auth/session";

// The connection header must always tell the user which of four distinct
// situations they are in: not yet logged in, actively connecting,
// connected, or rejected by the daemon. "Offline" alone is never shown
// for an auth problem.

export interface ConnectionBanner {
  /** Short state label for the header. */
  label: string;
  /** Longer explanation when the user has to act. */
  detail: string | null;
  /** True when the fix is enrolling (logging in) again. */
  needsEnrollment: boolean;
}

export function connectionBanner(
  conn: ConnPhase,
  auth: DeviceAuthStatus,
  options: { hasLegacySession: boolean; legacyRejected: boolean },
): ConnectionBanner {
  if (auth.kind === "rejected") {
    return {
      label: "rejected",
      detail: `access revoked: ${auth.reason} — log in again`,
      needsEnrollment: true,
    };
  }
  if (auth.kind === "unenrolled") {
    if (options.legacyRejected || !options.hasLegacySession) {
      return {
        label: "not logged in",
        detail: options.legacyRejected
          ? "The daemon rejected the stored session — log in again."
          : "Log in to connect this device.",
        needsEnrollment: true,
      };
    }
  }
  switch (conn) {
    case "connecting":
      return { label: "connecting…", detail: null, needsEnrollment: false };
    case "online":
      return { label: "connected", detail: null, needsEnrollment: false };
    case "offline":
      return { label: "offline — retrying", detail: null, needsEnrollment: false };
  }
}
