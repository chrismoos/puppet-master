import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";

import type { KeyValueStorage } from "@puppet-master/client-core/platform";

import { AuthHttpError, type JsonFetchLike } from "./api";
import { ALL_AUTH_KEYS, AUTH_KEYS, DeviceAuthSession, generateDeviceId } from "./session";

function memoryStorage(): KeyValueStorage & { map: Map<string, string> } {
  const map = new Map<string, string>();
  return {
    map,
    getItem: (key) => map.get(key) ?? null,
    setItem: (key, value) => void map.set(key, value),
  };
}

function tokensPayload(suffix: string, accessExpires = 10_000, refreshExpires = 100_000) {
  return {
    tokens: {
      accessToken: `acc-${suffix}`,
      accessTokenExpiresAtUnixMs: accessExpires,
      refreshToken: `ref-${suffix}`,
      refreshTokenExpiresAtUnixMs: refreshExpires,
    },
  };
}

function jsonResponse(status: number, payload: unknown) {
  return { status, json: () => Promise.resolve(payload) };
}

describe("generateDeviceId", () => {
  it("produces distinct 32-hex-character ids", () => {
    const a = generateDeviceId();
    const b = generateDeviceId();
    expect(a).toMatch(/^[0-9a-f]{32}$/);
    expect(a).not.toBe(b);
  });
});

describe("DeviceAuthSession", () => {
  it("generates the device id once and keeps it stable", () => {
    const storage = memoryStorage();
    const session = new DeviceAuthSession(storage, () => Promise.reject(new Error("no fetch")));
    const id = session.deviceId();
    expect(session.deviceId()).toBe(id);
    expect(storage.getItem(AUTH_KEYS.deviceId)).toBe(id);
  });

  it("enrolls with password proof and persists the token pair", async () => {
    const storage = memoryStorage();
    const bodies: string[] = [];
    const fetchImpl: JsonFetchLike = (_url, init) => {
      bodies.push(init.body ?? "");
      return Promise.resolve(
        jsonResponse(200, {
          device: { id: "1", name: "phone", platform: "ios" },
          ...tokensPayload("1"),
        }),
      );
    };
    const session = new DeviceAuthSession(storage, fetchImpl);
    expect(session.status().kind).toBe("unenrolled");
    await session.enroll(
      "https://pm.example",
      { username: "testuser", password: "hunter2" },
      { name: "phone", platform: "ios" },
    );
    expect(session.status().kind).toBe("enrolled");
    expect(session.accessToken()).toBe("acc-1");
    expect(storage.getItem(AUTH_KEYS.refreshToken)).toBe("ref-1");
    expect(JSON.parse(bodies[0]).deviceId).toBe(session.deviceId());
  });

  it("recovers a lost server device id from a refresh", async () => {
    const storage = memoryStorage();
    storage.map.set(AUTH_KEYS.refreshToken, "ref-old");
    // An enrolled device whose id went missing: status still reports
    // enrolled, so nothing would prompt the user to re-enroll.
    const fetchImpl: JsonFetchLike = () =>
      Promise.resolve(jsonResponse(200, { ...tokensPayload("2"), deviceId: "42" }));
    const session = new DeviceAuthSession(storage, fetchImpl);
    expect(session.serverDeviceId()).toBeNull();

    await session.freshAccessToken("http://controller");

    expect(session.serverDeviceId()).toBe("42");
  });

  it("keeps the stored device id when a refresh reports a different one", async () => {
    const storage = memoryStorage();
    storage.map.set(AUTH_KEYS.refreshToken, "ref-old");
    storage.map.set(AUTH_KEYS.serverDeviceId, "7");
    const fetchImpl: JsonFetchLike = () =>
      Promise.resolve(jsonResponse(200, { ...tokensPayload("2"), deviceId: "42" }));
    const session = new DeviceAuthSession(storage, fetchImpl);

    await session.freshAccessToken("http://controller");

    // Recovery fills a gap; it never reassigns a device that has an id.
    expect(session.serverDeviceId()).toBe("7");
  });

  it("refreshes normally against a daemon that omits the device id", async () => {
    const storage = memoryStorage();
    storage.map.set(AUTH_KEYS.refreshToken, "ref-old");
    const fetchImpl: JsonFetchLike = () =>
      Promise.resolve(jsonResponse(200, tokensPayload("2")));
    const session = new DeviceAuthSession(storage, fetchImpl);

    await session.freshAccessToken("http://controller");

    expect(session.accessToken()).toBe("acc-2");
    expect(session.serverDeviceId()).toBeNull();
  });

  it("stores the server device id from enrollment and clears it on sign-out", async () => {
    const storage = memoryStorage();
    const fetchImpl: JsonFetchLike = () =>
      Promise.resolve(
        jsonResponse(200, {
          device: { id: "42", name: "phone", platform: "ios" },
          ...tokensPayload("1"),
        }),
      );
    const session = new DeviceAuthSession(storage, fetchImpl);
    await session.enroll(
      "https://pm.example",
      { username: "testuser", password: "hunter2" },
      { name: "phone", platform: "ios" },
    );
    expect(session.serverDeviceId()).toBe("42");
    expect(storage.getItem(AUTH_KEYS.serverDeviceId)).toBe("42");
    session.signOut();
    expect(session.serverDeviceId()).toBeNull();
  });

  it("hydrates enrolled status from a stored refresh token", () => {
    const storage = memoryStorage();
    storage.setItem(AUTH_KEYS.refreshToken, "ref-stored");
    const session = new DeviceAuthSession(storage, () => Promise.reject(new Error("no fetch")));
    expect(session.status().kind).toBe("enrolled");
  });

  it("returns the stored access token while it is still fresh", async () => {
    const storage = memoryStorage();
    storage.setItem(AUTH_KEYS.accessToken, "acc-live");
    storage.setItem(AUTH_KEYS.accessExpiresAt, "100000");
    storage.setItem(AUTH_KEYS.refreshToken, "ref-live");
    const session = new DeviceAuthSession(
      storage,
      () => Promise.reject(new Error("must not refresh")),
      () => 50_000,
    );
    await expect(session.freshAccessToken("https://pm.example")).resolves.toBe("acc-live");
  });

  it("rotates through the refresh endpoint when the access token nears expiry", async () => {
    const storage = memoryStorage();
    storage.setItem(AUTH_KEYS.accessToken, "acc-old");
    storage.setItem(AUTH_KEYS.accessExpiresAt, "51000");
    storage.setItem(AUTH_KEYS.refreshToken, "ref-old");
    const bodies: string[] = [];
    const fetchImpl: JsonFetchLike = (_url, init) => {
      bodies.push(init.body ?? "");
      return Promise.resolve(jsonResponse(200, tokensPayload("new")));
    };
    const session = new DeviceAuthSession(storage, fetchImpl, () => 50_000);
    await expect(session.freshAccessToken("https://pm.example")).resolves.toBe("acc-new");
    expect(JSON.parse(bodies[0])).toEqual({ refreshToken: "ref-old" });
    expect(storage.getItem(AUTH_KEYS.refreshToken)).toBe("ref-new");
  });

  it("single-flights concurrent refreshes", async () => {
    const storage = memoryStorage();
    storage.setItem(AUTH_KEYS.refreshToken, "ref-old");
    let calls = 0;
    let release: (() => void) | null = null;
    const fetchImpl: JsonFetchLike = () => {
      calls += 1;
      return new Promise((resolve) => {
        release = () => resolve(jsonResponse(200, tokensPayload("new")));
      });
    };
    const session = new DeviceAuthSession(storage, fetchImpl);
    const first = session.refresh("https://pm.example");
    const second = session.refresh("https://pm.example");
    release?.();
    const [a, b] = await Promise.all([first, second]);
    expect(calls).toBe(1);
    expect(a.accessToken).toBe("acc-new");
    expect(b.accessToken).toBe("acc-new");
  });

  it("retries an authenticated call once after a 401 rotates the tokens", async () => {
    const storage = memoryStorage();
    storage.setItem(AUTH_KEYS.accessToken, "acc-stale");
    storage.setItem(AUTH_KEYS.accessExpiresAt, "100000");
    storage.setItem(AUTH_KEYS.refreshToken, "ref-old");
    const fetchImpl: JsonFetchLike = () => Promise.resolve(jsonResponse(200, tokensPayload("new")));
    const session = new DeviceAuthSession(storage, fetchImpl, () => 0);
    const attempts: string[] = [];
    const result = await session.withAccessToken("https://pm.example", (token) => {
      attempts.push(token);
      if (attempts.length === 1) {
        return Promise.reject(new AuthHttpError(401, "invalid or expired token"));
      }
      return Promise.resolve("ok");
    });
    expect(result).toBe("ok");
    expect(attempts).toEqual(["acc-stale", "acc-new"]);
  });

  it("moves to a visible rejected state when the daemon refuses the refresh", async () => {
    const storage = memoryStorage();
    storage.setItem(AUTH_KEYS.accessToken, "acc-old");
    storage.setItem(AUTH_KEYS.refreshToken, "ref-dead");
    const fetchImpl: JsonFetchLike = () =>
      Promise.resolve(jsonResponse(401, { error: "invalid or expired token" }));
    const session = new DeviceAuthSession(storage, fetchImpl, () => 0);
    let notified = 0;
    session.subscribe(() => {
      notified += 1;
    });
    await expect(session.refresh("https://pm.example")).rejects.toMatchObject({
      name: "AuthRejectedError",
      message: "invalid or expired token",
    });
    expect(session.status()).toEqual({
      kind: "rejected",
      reason: "invalid or expired token",
      status: 401,
      step: "refresh",
    });
    expect(notified).toBe(1);
    expect(session.accessToken()).toBeNull();
    expect(storage.getItem(AUTH_KEYS.refreshToken)).toBe("");
  });

  /**
   * A proxy, a gateway or a captive portal can answer 401 for the daemon,
   * and it is not the daemon's verdict on this device. Dropping the
   * credentials on one makes the user enroll again because something in
   * the way said no.
   */
  it("keeps the device enrolled when a 401 did not come from the daemon", async () => {
    const storage = memoryStorage();
    storage.setItem(AUTH_KEYS.accessToken, "acc-old");
    storage.setItem(AUTH_KEYS.refreshToken, "ref-live");
    const fetchImpl: JsonFetchLike = () =>
      Promise.resolve(jsonResponse(401, { message: "Unauthorized" }));
    const session = new DeviceAuthSession(storage, fetchImpl, () => 0);

    await expect(session.refresh("https://pm.example")).rejects.toMatchObject({
      name: "AuthHttpError",
      status: 401,
    });
    expect(session.status().kind).toBe("enrolled");
    expect(storage.getItem(AUTH_KEYS.refreshToken)).toBe("ref-live");
  });

  /// A refusal the device can recover from still has to be reportable:
  /// which credential, what the daemon answered, and its status.
  it("keeps the last refusal with its status", async () => {
    const storage = memoryStorage();
    storage.setItem(AUTH_KEYS.refreshToken, "ref-live");
    const fetchImpl: JsonFetchLike = () =>
      Promise.resolve(jsonResponse(500, { error: "internal auth error" }));
    const session = new DeviceAuthSession(storage, fetchImpl, () => 0);

    await expect(session.refresh("https://pm.example")).rejects.toThrow(/internal auth error/);
    expect(session.lastFailure()).toEqual({
      step: "refresh",
      status: 500,
      reason: "internal auth error",
    });
    expect(session.status().kind).toBe("enrolled");
  });

  it("reports a refresh that never answered without a status", async () => {
    const storage = memoryStorage();
    storage.setItem(AUTH_KEYS.refreshToken, "ref-live");
    const fetchImpl: JsonFetchLike = () => new Promise(() => {});
    const session = new DeviceAuthSession(storage, fetchImpl, Date.now, 1);

    await expect(session.refresh("https://pm.example")).rejects.toThrow(/timed out/);
    expect(session.lastFailure()).toMatchObject({ step: "refresh", status: 0 });
  });

  it("reports the daemon refusing the access token on a socket", () => {
    const storage = memoryStorage();
    const session = new DeviceAuthSession(storage, () => Promise.reject(new Error("no fetch")));
    let notified = 0;
    session.subscribe(() => {
      notified += 1;
    });
    session.accessRefused();
    expect(session.lastFailure()).toMatchObject({ step: "access", status: 401 });
    expect(notified).toBe(1);
  });

  /**
   * An access token inside the expiry slack on arrival cannot have come
   * from a clock this device agrees with. Without measuring it the app
   * refreshes without pause and reports the symptom instead of the cause.
   */
  it("measures a daemon clock that trails this device", async () => {
    const storage = memoryStorage();
    storage.setItem(AUTH_KEYS.refreshToken, "ref-live");
    // The daemon says the token expires at 900s, a quarter hour of life.
    const fetchImpl: JsonFetchLike = () =>
      Promise.resolve(jsonResponse(200, tokensPayload("new", 900_000, 90_000_000)));

    const agreeing = new DeviceAuthSession(storage, fetchImpl, () => 0);
    await agreeing.refresh("https://pm.example");
    expect(agreeing.clockDisagreement()).toBe(0);

    // This device reads 1000s, so the token it was just handed expired
    // a hundred seconds ago by its own clock.
    const skewed = new DeviceAuthSession(storage, fetchImpl, () => 1_000_000);
    await skewed.refresh("https://pm.example");
    expect(skewed.clockDisagreement()).toBe(130_000);
  });

  it("forgets a refusal once a refresh succeeds", async () => {
    const storage = memoryStorage();
    storage.setItem(AUTH_KEYS.refreshToken, "ref-live");
    let answers = 0;
    const fetchImpl: JsonFetchLike = () => {
      answers += 1;
      return Promise.resolve(
        answers === 1
          ? jsonResponse(500, { error: "internal auth error" })
          : jsonResponse(200, tokensPayload("new")),
      );
    };
    const session = new DeviceAuthSession(storage, fetchImpl, () => 0);

    await expect(session.refresh("https://pm.example")).rejects.toThrow();
    expect(session.lastFailure()).not.toBeNull();
    await session.refresh("https://pm.example");
    expect(session.lastFailure()).toBeNull();
  });

  it("keeps tokens and enrolled status through a network refresh failure", async () => {
    const storage = memoryStorage();
    storage.setItem(AUTH_KEYS.refreshToken, "ref-live");
    const session = new DeviceAuthSession(storage, () => Promise.reject(new Error("offline")));
    await expect(session.refresh("https://pm.example")).rejects.toThrow(/offline/);
    expect(session.status().kind).toBe("enrolled");
    expect(storage.getItem(AUTH_KEYS.refreshToken)).toBe("ref-live");
    // A later refresh is a new flight, not the cached rejection.
    await expect(session.refresh("https://pm.example")).rejects.toThrow(/offline/);
  });

  it("signOut clears tokens but keeps the device id", () => {
    const storage = memoryStorage();
    storage.setItem(AUTH_KEYS.refreshToken, "ref-live");
    const session = new DeviceAuthSession(storage, () => Promise.reject(new Error("no fetch")));
    const id = session.deviceId();
    session.signOut();
    expect(session.status().kind).toBe("unenrolled");
    expect(storage.getItem(AUTH_KEYS.refreshToken)).toBe("");
    expect(session.deviceId()).toBe(id);
  });

  it("enrolls again after signOut under the same device id", async () => {
    const storage = memoryStorage();
    const bodies: string[] = [];
    let enrollment = 0;
    const fetchImpl: JsonFetchLike = (_url, init) => {
      bodies.push(init.body ?? "");
      enrollment += 1;
      return Promise.resolve(
        jsonResponse(200, {
          device: { id: "1", name: "phone", platform: "ios" },
          ...tokensPayload(String(enrollment)),
        }),
      );
    };
    const session = new DeviceAuthSession(storage, fetchImpl);
    const login = () =>
      session.enroll(
        "https://pm.example",
        { username: "testuser", password: "hunter2" },
        { name: "phone", platform: "ios" },
      );

    await login();
    session.signOut();
    await login();

    // The controller correlates an enrollment to a device by this id, so
    // logging back in must report the id the installation already used
    // or it registers as a second device.
    const sent = bodies.map((body) => JSON.parse(body).deviceId);
    expect(sent[1]).toBe(sent[0]);
    expect(sent[1]).toBe(session.deviceId());
  });

  it("persists device name and enrolment timestamp on enroll", async () => {
    const storage = memoryStorage();
    const now = 1700000000000;
    const fetchImpl: JsonFetchLike = () =>
      Promise.resolve(
        jsonResponse(200, {
          device: { id: "1", name: "phone", platform: "ios" },
          ...tokensPayload("1"),
        }),
      );
    const session = new DeviceAuthSession(storage, fetchImpl, () => now);
    await session.enroll(
      "https://pm.example",
      { username: "u", password: "p" },
      { name: "My Phone", platform: "ios" },
    );
    expect(session.deviceName()).toBe("My Phone");
    expect(session.enrolledAt()).toBe(now);
    expect(storage.getItem(AUTH_KEYS.deviceName)).toBe("My Phone");
    expect(storage.getItem(AUTH_KEYS.enrolledAt)).toBe(String(now));
  });

  it("returns null for deviceName and enrolledAt before enrolment", () => {
    const storage = memoryStorage();
    const session = new DeviceAuthSession(storage, () => Promise.reject(new Error("no fetch")));
    expect(session.deviceName()).toBeNull();
    expect(session.enrolledAt()).toBeNull();
  });

  it("exposes every persisted key for secure-store hydration", () => {
    expect(ALL_AUTH_KEYS).toEqual([
      AUTH_KEYS.deviceId,
      AUTH_KEYS.serverDeviceId,
      AUTH_KEYS.accessToken,
      AUTH_KEYS.accessExpiresAt,
      AUTH_KEYS.refreshToken,
      AUTH_KEYS.refreshExpiresAt,
      AUTH_KEYS.deviceName,
      AUTH_KEYS.enrolledAt,
    ]);
  });

  it("times out a stalled refresh and keeps tokens valid for retry", async () => {
    const storage = memoryStorage();
    storage.setItem(AUTH_KEYS.refreshToken, "ref-live");
    const fetchImpl: JsonFetchLike = () =>
      new Promise(() => {
        // Never resolves, simulating a suspended request.
      });
    // Use a 1 ms timeout so the test completes instantly.
    const session = new DeviceAuthSession(storage, fetchImpl, Date.now, 1);
    await expect(session.refresh("https://pm.example")).rejects.toThrow(/timed out/);
    expect(session.status().kind).toBe("enrolled");
    expect(storage.getItem(AUTH_KEYS.refreshToken)).toBe("ref-live");
  });

  /**
   * The daemon accepts a refresh token once and tolerates one repeat. A
   * client that gave up waiting and presented it again would spend that
   * repeat, and the attempt after it revokes the device, which reads out
   * as being logged out with the token reported invalid.
   */
  it("does not present the same refresh token again while an attempt is outstanding", async () => {
    const storage = memoryStorage();
    storage.setItem(AUTH_KEYS.refreshToken, "ref-live");
    let presentations = 0;
    let answer: (() => void) | null = null;
    const fetchImpl: JsonFetchLike = () => {
      presentations += 1;
      return new Promise((resolve) => {
        answer = () => resolve(jsonResponse(200, tokensPayload("new")));
      });
    };
    const session = new DeviceAuthSession(storage, fetchImpl, Date.now, 1);

    await expect(session.refresh("https://pm.example")).rejects.toThrow(/timed out/);
    await expect(session.refresh("https://pm.example")).rejects.toThrow(/timed out/);
    expect(presentations).toBe(1);

    answer?.();
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(storage.getItem(AUTH_KEYS.refreshToken)).toBe("ref-new");
    expect(presentations).toBe(1);
  });

  /// Giving up waiting is not giving up on the answer: the pair a slow
  /// rotation returns is the only one the daemon will still accept.
  it("keeps the pair a refresh returns after the caller stopped waiting", async () => {
    const storage = memoryStorage();
    storage.setItem(AUTH_KEYS.refreshToken, "ref-live");
    let answer: (() => void) | null = null;
    const fetchImpl: JsonFetchLike = () =>
      new Promise((resolve) => {
        answer = () => resolve(jsonResponse(200, tokensPayload("late")));
      });
    const session = new DeviceAuthSession(storage, fetchImpl, Date.now, 1);

    await expect(session.refresh("https://pm.example")).rejects.toThrow(/timed out/);
    answer?.();
    await new Promise((resolve) => setTimeout(resolve, 0));

    expect(storage.getItem(AUTH_KEYS.accessToken)).toBe("acc-late");
    expect(session.accessToken()).toBe("acc-late");
    expect(session.status().kind).toBe("enrolled");
  });

  it("writes refresh token before access token when storing rotated tokens", async () => {
    const writes: string[] = [];
    const map = new Map<string, string>();
    map.set(AUTH_KEYS.refreshToken, "ref-old");
    const storage = {
      getItem: (key: string) => map.get(key) ?? null,
      setItem: (key: string, value: string) => {
        map.set(key, value);
        writes.push(key);
      },
    };
    const fetchImpl: JsonFetchLike = () =>
      Promise.resolve(jsonResponse(200, tokensPayload("new")));
    const session = new DeviceAuthSession(storage, fetchImpl);
    await session.refresh("https://pm.example");
    const rtIdx = writes.indexOf(AUTH_KEYS.refreshToken);
    const atIdx = writes.indexOf(AUTH_KEYS.accessToken);
    expect(rtIdx).toBeGreaterThanOrEqual(0);
    expect(atIdx).toBeGreaterThanOrEqual(0);
    expect(rtIdx).toBeLessThan(atIdx);
  });

  it("awaits storage flush before returning the rotated pair", async () => {
    const storage = memoryStorage();
    storage.setItem(AUTH_KEYS.refreshToken, "ref-old");
    let flushCalled = false;
    (storage as Record<string, unknown>).flush = () => {
      flushCalled = true;
      return Promise.resolve();
    };
    const fetchImpl: JsonFetchLike = () =>
      Promise.resolve(jsonResponse(200, tokensPayload("new")));
    const session = new DeviceAuthSession(storage, fetchImpl);
    await session.refresh("https://pm.example");
    expect(flushCalled).toBe(true);
  });
});

describe("proactive token refresh", () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());

  it("schedules a refresh about 60 s before the access token expires", async () => {
    const storage = memoryStorage();
    let now = 0;
    storage.setItem(AUTH_KEYS.refreshToken, "ref");
    // Access token expires at 120_000, so proactive refresh at 60_000
    storage.setItem(AUTH_KEYS.accessToken, "acc");
    storage.setItem(AUTH_KEYS.accessExpiresAt, "120000");

    let refreshCount = 0;
    const fetchImpl: JsonFetchLike = () => {
      refreshCount++;
      return Promise.resolve(jsonResponse(200, tokensPayload("new", now + 120_000)));
    };
    const session = new DeviceAuthSession(storage, fetchImpl, () => now);

    session.startProactiveRefresh("https://pm.example");

    // At now=0, refresh is scheduled for 60_000 (120_000 - 60_000)
    await vi.advanceTimersByTimeAsync(59_999);
    expect(refreshCount).toBe(0);
    await vi.advanceTimersByTimeAsync(1);
    expect(refreshCount).toBe(1);

    session.stopProactiveRefresh();
  });

  it("reschedules after a successful refresh", async () => {
    const storage = memoryStorage();
    let now = 0;
    storage.setItem(AUTH_KEYS.refreshToken, "ref");
    storage.setItem(AUTH_KEYS.accessToken, "acc");
    storage.setItem(AUTH_KEYS.accessExpiresAt, "120000");

    let refreshCount = 0;
    const fetchImpl: JsonFetchLike = () => {
      refreshCount++;
      // New token expires 120s from "now"
      return Promise.resolve(jsonResponse(200, tokensPayload(`new-${refreshCount}`, now + 120_000)));
    };
    const session = new DeviceAuthSession(storage, fetchImpl, () => now);

    session.startProactiveRefresh("https://pm.example");

    // First refresh at 60_000
    now = 60_000;
    await vi.advanceTimersByTimeAsync(60_000);
    expect(refreshCount).toBe(1);

    // After refresh, new expiry is 60_000 + 120_000 = 180_000. The lead
    // is a quarter of the lifetime the daemon issued, so the next one is
    // at 180_000 - 30_000, which is 90_000 from now.
    now = 150_000;
    await vi.advanceTimersByTimeAsync(90_000);
    expect(refreshCount).toBe(2);

    session.stopProactiveRefresh();
  });

  /**
   * The lead is bounded by the lifetime the daemon issues, which is a
   * setting. A lifetime shorter than a fixed lead would put every token
   * inside it on arrival, and the app would refresh the moment it stored
   * one, rotating its refresh token without pause.
   */
  it("does not refresh at once when the daemon issues a short-lived token", async () => {
    const storage = memoryStorage();
    let now = 0;
    storage.setItem(AUTH_KEYS.refreshToken, "ref");
    let refreshCount = 0;
    const fetchImpl: JsonFetchLike = () => {
      refreshCount += 1;
      return Promise.resolve(
        jsonResponse(200, tokensPayload(`new-${refreshCount}`, now + 60_000)),
      );
    };
    const session = new DeviceAuthSession(storage, fetchImpl, () => now);

    await session.refresh("https://pm.example");
    expect(refreshCount).toBe(1);
    session.startProactiveRefresh("https://pm.example");

    // A minute of life, so nothing is due for the first 45 seconds of it.
    await vi.advanceTimersByTimeAsync(40_000);
    expect(refreshCount).toBe(1);

    // A token stored a moment ago is also still good enough to use.
    now = 1_000;
    await expect(session.freshAccessToken("https://pm.example")).resolves.toBe("acc-new-1");
    expect(refreshCount).toBe(1);

    now = 45_000;
    await vi.advanceTimersByTimeAsync(5_000);
    expect(refreshCount).toBe(2);

    session.stopProactiveRefresh();
  });

  it("stops the timer on stopProactiveRefresh", async () => {
    const storage = memoryStorage();
    storage.setItem(AUTH_KEYS.refreshToken, "ref");
    storage.setItem(AUTH_KEYS.accessToken, "acc");
    storage.setItem(AUTH_KEYS.accessExpiresAt, "120000");

    let refreshCount = 0;
    const fetchImpl: JsonFetchLike = () => {
      refreshCount++;
      return Promise.resolve(jsonResponse(200, tokensPayload("new")));
    };
    const session = new DeviceAuthSession(storage, fetchImpl, () => 0);

    session.startProactiveRefresh("https://pm.example");
    session.stopProactiveRefresh();

    await vi.advanceTimersByTimeAsync(120_000);
    expect(refreshCount).toBe(0);
  });

  it("retries after 10 s on network failure", async () => {
    const storage = memoryStorage();
    storage.setItem(AUTH_KEYS.refreshToken, "ref");
    storage.setItem(AUTH_KEYS.accessToken, "acc");
    storage.setItem(AUTH_KEYS.accessExpiresAt, "60000");

    let callCount = 0;
    const fetchImpl: JsonFetchLike = () => {
      callCount++;
      if (callCount === 1) return Promise.reject(new Error("offline"));
      return Promise.resolve(jsonResponse(200, tokensPayload("recovered", 120_000)));
    };
    const session = new DeviceAuthSession(storage, fetchImpl, () => 0);

    session.startProactiveRefresh("https://pm.example");

    // Proactive fires immediately (60_000 - 60_000 = 0)
    await vi.advanceTimersByTimeAsync(1);
    expect(callCount).toBe(1);

    // Retry after 10 s (advance and let promise chains settle)
    await vi.advanceTimersByTimeAsync(10_001);
    expect(callCount).toBe(2);

    session.stopProactiveRefresh();
  });
});
