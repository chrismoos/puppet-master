import type { ConnPhase } from "@puppet-master/client-core/state/reducer";

import type { AuthFailure, AuthStep, DeviceAuthStatus } from "./auth/session";

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

/** Which credential failed, in the words the user can act on. */
function attempt(step: AuthStep): string {
  return step === "refresh" ? "refreshing the device token" : "using the access token";
}

/** What the daemon answered, with its status code when it sent one. */
function describeFailure(failure: AuthFailure): string {
  const status = failure.status > 0 ? ` (${failure.status})` : "";
  return `${attempt(failure.step)} failed${status}: ${failure.reason}`;
}

/**
 * A daemon whose clock trails this device's issues tokens that are spent
 * before they arrive, so the app refreshes without pause and every other
 * explanation is a symptom. Said first for that reason.
 */
function clockNote(disagreementMs: number): string | null {
  if (disagreementMs <= 0) return null;
  const minutes = Math.max(1, Math.round(disagreementMs / 60_000));
  return `the daemon's clock is about ${minutes} ${
    minutes === 1 ? "minute" : "minutes"
  } behind this device's, so tokens arrive already expired`;
}

export function connectionBanner(
  conn: ConnPhase,
  auth: DeviceAuthStatus,
  options: {
    hasLegacySession: boolean;
    legacyRejected: boolean;
    failure?: AuthFailure | null;
    clockDisagreementMs?: number;
  },
): ConnectionBanner {
  const clock = clockNote(options.clockDisagreementMs ?? 0);
  if (auth.kind === "rejected") {
    const refusal = describeFailure({
      step: auth.step,
      status: auth.status,
      reason: auth.reason,
    });
    return {
      label: "rejected",
      detail: clock
        ? `${refusal} — ${clock}. Log in again.`
        : `${refusal} — log in again`,
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
  const trouble = [options.failure ? describeFailure(options.failure) : null, clock]
    .filter((part): part is string => part !== null)
    .join(" — ");
  switch (conn) {
    case "connecting":
      return { label: "connecting…", detail: trouble || null, needsEnrollment: false };
    case "online":
      return { label: "connected", detail: clock, needsEnrollment: false };
    case "offline":
      return {
        label: "offline — retrying",
        detail: trouble || null,
        needsEnrollment: false,
      };
  }
}
