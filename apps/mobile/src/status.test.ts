import { describe, expect, it } from "vitest";

import { connectionBanner } from "./status";

const NO_LEGACY = { hasLegacySession: false, legacyRejected: false };

describe("connectionBanner", () => {
  it("asks for login when the device is not enrolled", () => {
    const banner = connectionBanner("offline", { kind: "unenrolled" }, NO_LEGACY);
    expect(banner.label).toBe("not logged in");
    expect(banner.needsEnrollment).toBe(true);
  });

  it("shows a rejected state with the daemon's reason", () => {
    const banner = connectionBanner(
      "offline",
      { kind: "rejected", reason: "invalid or expired token" },
      NO_LEGACY,
    );
    expect(banner.label).toBe("rejected");
    expect(banner.detail).toContain("invalid or expired token");
    expect(banner.needsEnrollment).toBe(true);
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
