import { describe, expect, it } from "vitest";

import { connectionBanner } from "./status";

const NO_LEGACY = { hasLegacySession: false, legacyRejected: false };

describe("connectionBanner", () => {
  it("asks for login when the device is not enrolled", () => {
    const banner = connectionBanner("offline", { kind: "unenrolled" }, NO_LEGACY);
    expect(banner.label).toBe("not logged in");
    expect(banner.needsEnrollment).toBe(true);
  });

  /// Which credential failed decides what the user does about it, and
  /// the status code is what makes a report of this worth anything.
  it("names the credential that was refused, and the status", () => {
    const banner = connectionBanner(
      "offline",
      { kind: "rejected", reason: "invalid or expired token", status: 401, step: "refresh" },
      NO_LEGACY,
    );
    expect(banner.label).toBe("rejected");
    expect(banner.detail).toContain("refreshing the device token");
    expect(banner.detail).toContain("(401)");
    expect(banner.detail).toContain("invalid or expired token");
    expect(banner.needsEnrollment).toBe(true);
  });

  it("tells a refused access token apart from a refused refresh", () => {
    const banner = connectionBanner(
      "offline",
      { kind: "rejected", reason: "no", status: 403, step: "access" },
      NO_LEGACY,
    );
    expect(banner.detail).toContain("using the access token");
    expect(banner.detail).toContain("(403)");
  });

  /// A device that is still enrolled says why it is not connected, so a
  /// refusal that it can recover from is not reported as "offline" alone.
  it("says what failed while it is still retrying", () => {
    const banner = connectionBanner("offline", { kind: "enrolled" }, {
      ...NO_LEGACY,
      failure: { step: "refresh", status: 500, reason: "internal auth error" },
    });
    expect(banner.label).toBe("offline — retrying");
    expect(banner.detail).toContain("refreshing the device token failed (500)");
    expect(banner.detail).toContain("internal auth error");
  });

  it("reports a refusal with no answer without inventing a status", () => {
    const banner = connectionBanner("offline", { kind: "enrolled" }, {
      ...NO_LEGACY,
      failure: { step: "refresh", status: 0, reason: "the daemon did not answer within 15s" },
    });
    expect(banner.detail).toBe("refreshing the device token failed: the daemon did not answer within 15s");
  });

  /// Clocks that disagree expire every token on arrival, which looks like
  /// a credential problem and is not one.
  it("reports a daemon clock that trails this device", () => {
    const banner = connectionBanner("online", { kind: "enrolled" }, {
      ...NO_LEGACY,
      clockDisagreementMs: 20 * 60_000,
    });
    expect(banner.detail).toContain("20 minutes behind");
    const rejected = connectionBanner(
      "offline",
      { kind: "rejected", reason: "invalid or expired token", status: 401, step: "refresh" },
      { ...NO_LEGACY, clockDisagreementMs: 20 * 60_000 },
    );
    expect(rejected.detail).toContain("20 minutes behind");
    expect(rejected.detail).toContain("(401)");
  });

  it("reports connecting, connected, and offline for an enrolled device", () => {
    const enrolled = { kind: "enrolled" } as const;
    expect(connectionBanner("connecting", enrolled, NO_LEGACY).label).toBe("connecting…");
    expect(connectionBanner("online", enrolled, NO_LEGACY).label).toBe("connected");
    const offline = connectionBanner("offline", enrolled, NO_LEGACY);
    expect(offline.label).toBe("offline — retrying");
    expect(offline.needsEnrollment).toBe(false);
  });

  it("lets a stored legacy session keep connecting without enrollment", () => {
    const banner = connectionBanner(
      "online",
      { kind: "unenrolled" },
      { hasLegacySession: true, legacyRejected: false },
    );
    expect(banner.label).toBe("connected");
    expect(banner.needsEnrollment).toBe(false);
  });

  it("asks for login once the daemon rejects the legacy session", () => {
    const banner = connectionBanner(
      "offline",
      { kind: "unenrolled" },
      { hasLegacySession: true, legacyRejected: true },
    );
    expect(banner.label).toBe("not logged in");
    expect(banner.detail).toContain("rejected the stored session");
    expect(banner.needsEnrollment).toBe(true);
  });
});
