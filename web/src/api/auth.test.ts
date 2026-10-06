import { afterEach, describe, expect, it, vi } from "vitest";
import { changePassword, fetchVersion, setup } from "./auth";

function jsonResponse(body: unknown, init?: { status?: number }): Response {
  return new Response(JSON.stringify(body), {
    status: init?.status ?? 200,
    headers: { "Content-Type": "application/json" },
  });
}

function stubFetch(response: Response) {
  const fetchMock = vi.fn(() => Promise.resolve(response));
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe("fetchVersion", () => {
  /// The dashboard says beside a preview's link whether the preview runs on
  /// this origin, so it has to read the mount the daemon reports.
  it("reads the forward mount out of the nested object the daemon sends", async () => {
    stubFetch(
      jsonResponse({
        version: "0.9.0",
        gitRev: "abc1234",
        hostPlanePort: 7677,
        installCommand: "curl -fsSL https://dl.example/install.sh | sh",
        platform: "linux",
        forwardMount: { mode: "path-prefix" },
      }),
    );
    await expect(fetchVersion()).resolves.toEqual({
      version: "0.9.0",
      gitRev: "abc1234",
      channel: "stable",
      hostPlanePort: 7677,
      installCommand: "curl -fsSL https://dl.example/install.sh | sh",
      platform: "linux",
      forwardMount: "path-prefix",
      publicUrl: null,
    });
  });

  it("carries an isolating mount through unchanged", async () => {
    stubFetch(
      jsonResponse({
        version: "0.9.0",
        forwardMount: { mode: "share-domain", shareDomain: "previews.example" },
      }),
    );
    await expect(fetchVersion()).resolves.toMatchObject({
      forwardMount: "share-domain",
    });
  });

  /// An older daemon reports no mount. Absent must not read as path-prefix, or
  /// the dashboard would warn about an origin it does not know it shares.
  it("reports an absent mount as unknown rather than guessing", async () => {
    stubFetch(jsonResponse({ version: "0.9.0", gitRev: "abc1234" }));
    await expect(fetchVersion()).resolves.toMatchObject({ forwardMount: null });
  });

  it("answers null when the daemon cannot be reached", async () => {
    vi.stubGlobal("fetch", vi.fn(() => Promise.reject(new Error("offline"))));
    await expect(fetchVersion()).resolves.toBeNull();
  });
});

describe("setup", () => {
  /// A refused password reached the screen as the daemon's own JSON, so the
  /// first-run form showed {"error":"..."} where a sentence belongs.
  it("reads the daemon's reason out of a rejected password", async () => {
    stubFetch(
      jsonResponse({ error: "password must be at least 8 characters" }, { status: 400 }),
    );
    await expect(setup("testuser", "short")).rejects.toThrow(
      "password must be at least 8 characters",
    );
  });

  it("names both fields when the credential itself is refused", async () => {
    stubFetch(jsonResponse({ error: "invalid credentials" }, { status: 401 }));
    await expect(setup("testuser", "whatever")).rejects.toThrow("invalid username or password");
  });

  it("falls back to the status when the body carries no reason", async () => {
    stubFetch(new Response("not json at all", { status: 500 }));
    await expect(setup("testuser", "whatever")).rejects.toThrow("request failed (500)");
  });
});

describe("changePassword", () => {
  it("reads the daemon's reason the same way", async () => {
    stubFetch(
      jsonResponse({ error: "new password must differ from the current one" }, { status: 400 }),
    );
    await expect(changePassword("old", "old")).rejects.toThrow(
      "new password must differ from the current one",
    );
  });
});
