import { describe, expect, it } from "vitest";

import { openableForwardUrl } from "@puppet-master/client-core/api/forwards";

import {
  AuthHttpError,
  bearerJsonFetch,
  enrollDevice,
  mintControlSocketTicket,
  mintTerminalAttachTicket,
  refreshTokens,
  registerPushEndpoint,
  type JsonFetchLike,
} from "./api";

interface Request {
  url: string;
  method: string;
  headers: Record<string, string>;
  body: string | undefined;
}

function fetchStub(
  status: number,
  payload: unknown,
  requests: Request[] = [],
): JsonFetchLike {
  return (url, init) => {
    requests.push({ url, method: init.method, headers: init.headers, body: init.body });
    return Promise.resolve({ status, json: () => Promise.resolve(payload) });
  };
}

const TOKENS = {
  accessToken: "acc-1",
  accessTokenExpiresAtUnixMs: 1_000,
  refreshToken: "ref-1",
  refreshTokenExpiresAtUnixMs: 2_000,
};

describe("enrollDevice", () => {
  it("posts password proof and device metadata in the body", async () => {
    const requests: Request[] = [];
    const enrollment = await enrollDevice(
      "https://pm.example/",
      {
        deviceId: "dev-1",
        name: "test phone",
        platform: "ios",
        proof: { username: "testuser", password: "hunter2" },
      },
      fetchStub(
        200,
        {
          device: { id: "7", name: "test phone", platform: "ios" },
          tokens: TOKENS,
          installationId: "inst-1",
        },
        requests,
      ),
    );
    expect(requests[0].url).toBe("https://pm.example/api/mobile/devices/enroll");
    expect(JSON.parse(requests[0].body ?? "")).toEqual({
      deviceId: "dev-1",
      name: "test phone",
      platform: "ios",
      username: "testuser",
      password: "hunter2",
    });
    expect(requests[0].url).not.toContain("hunter2");
    expect(enrollment.device.id).toBe("7");
    expect(enrollment.tokens).toEqual(TOKENS);
    expect(enrollment.installationId).toBe("inst-1");
  });

  it("posts an enrollment token proof without credentials", async () => {
    const requests: Request[] = [];
    await enrollDevice(
      "https://pm.example",
      { deviceId: "dev-1", name: "", platform: "android", proof: { enrollToken: "tok-1" } },
      fetchStub(
        200,
        { device: { id: "8", name: "", platform: "android" }, tokens: TOKENS },
        requests,
      ),
    );
    const body = JSON.parse(requests[0].body ?? "");
    expect(body.enrollToken).toBe("tok-1");
    expect(body.username).toBeUndefined();
    expect(body.password).toBeUndefined();
  });

  it("raises the daemon's error message with the status", async () => {
    await expect(
      enrollDevice(
        "https://pm.example",
        { deviceId: "dev-1", name: "", platform: "ios", proof: { enrollToken: "used" } },
        fetchStub(401, { error: "enrollment token already used" }),
      ),
    ).rejects.toMatchObject({
      name: "AuthHttpError",
      status: 401,
      message: "enrollment token already used",
    });
  });

  it("rejects a malformed token payload", async () => {
    await expect(
      enrollDevice(
        "https://pm.example",
        { deviceId: "dev-1", name: "", platform: "ios", proof: { enrollToken: "t" } },
        fetchStub(200, { device: { id: "1", name: "", platform: "ios" }, tokens: {} }),
      ),
    ).rejects.toThrow(/malformed token response/);
  });
});

describe("refreshTokens", () => {
  it("posts the refresh token in the body and returns the rotated pair", async () => {
    const requests: Request[] = [];
    const refreshed = await refreshTokens(
      "https://pm.example",
      "ref-old",
      fetchStub(200, { tokens: TOKENS, deviceId: "42" }, requests),
    );
    expect(requests[0].url).toBe("https://pm.example/api/mobile/devices/refresh");
    expect(JSON.parse(requests[0].body ?? "")).toEqual({ refreshToken: "ref-old" });
    expect(refreshed.tokens).toEqual(TOKENS);
    expect(refreshed.deviceId).toBe("42");
  });

  it("reports no device id when the daemon does not send one", async () => {
    const refreshed = await refreshTokens(
      "https://pm.example",
      "ref-old",
      fetchStub(200, { tokens: TOKENS }),
    );
    expect(refreshed.tokens).toEqual(TOKENS);
    expect(refreshed.deviceId).toBeNull();
  });

  it("propagates a 401 as AuthHttpError", async () => {
    await expect(
      refreshTokens("https://pm.example", "ref-dead", fetchStub(401, { error: "invalid or expired token" })),
    ).rejects.toSatisfy((err: unknown) => err instanceof AuthHttpError && err.status === 401);
  });
});

describe("socket ticket minting", () => {
  it("mints a control ticket with only the bearer header", async () => {
    const requests: Request[] = [];
    const ticket = await mintControlSocketTicket(
      "https://pm.example",
      "acc-1",
      fetchStub(200, { ticket: "tick-1", expiresAtUnixMs: 99 }, requests),
    );
    expect(requests[0].url).toBe("https://pm.example/api/ws/ticket");
    expect(requests[0].headers["Authorization"]).toBe("Bearer acc-1");
    expect(requests[0].body).toBeUndefined();
    expect(requests[0].url).not.toContain("acc-1");
    expect(ticket).toEqual({ ticket: "tick-1", expiresAtUnixMs: 99 });
  });

  it("mints a terminal attach ticket bound to the generation", async () => {
    const requests: Request[] = [];
    const ticket = await mintTerminalAttachTicket(
      "https://pm.example",
      "acc-1",
      "42",
      "3",
      fetchStub(200, { ticket: "tick-2", expiresAtUnixMs: 100 }, requests),
      65536,
    );
    expect(requests[0].url).toBe("https://pm.example/api/terminals/42/attach-ticket");
    expect(requests[0].headers["Authorization"]).toBe("Bearer acc-1");
    expect(JSON.parse(requests[0].body ?? "")).toEqual({ generation: "3", replayBytes: 65536 });
    expect(ticket.ticket).toBe("tick-2");
  });

  it("omits replayBytes when the caller does not bound it", async () => {
    const requests: Request[] = [];
    await mintTerminalAttachTicket(
      "https://pm.example",
      "acc-1",
      "42",
      "3",
      fetchStub(200, { ticket: "t", expiresAtUnixMs: 1 }, requests),
    );
    expect(JSON.parse(requests[0].body ?? "")).toEqual({ generation: "3" });
  });

  it("reports a stale generation with the conflict status", async () => {
    await expect(
      mintTerminalAttachTicket(
        "https://pm.example",
        "acc-1",
        "42",
        "2",
        fetchStub(409, { error: "terminal generation changed" }),
      ),
    ).rejects.toMatchObject({ status: 409, message: "terminal generation changed" });
  });
});

describe("registerPushEndpoint", () => {
  it("sends a PUT with the push registration body to the device push path", async () => {
    const requests: Request[] = [];
    await registerPushEndpoint(
      "https://pm.example",
      "acc-1",
      "42",
      {
        token: "deadbeef",
        environment: "sandbox",
        locale: "en-US",
        previewsEnabled: false,
      },
      fetchStub(200, { endpoint: {} }, requests),
    );
    expect(requests[0].url).toBe("https://pm.example/api/mobile/devices/42/push");
    expect(requests[0].method).toBe("PUT");
    expect(requests[0].headers["Authorization"]).toBe("Bearer acc-1");
    const body = JSON.parse(requests[0].body ?? "");
    expect(body.provider).toBeUndefined();
    expect(body.token).toBe("deadbeef");
    expect(body.environment).toBe("sandbox");
    expect(body.locale).toBe("en-US");
    expect(body.previewsEnabled).toBe(false);
  });

  it("raises a 400 as AuthHttpError", async () => {
    await expect(
      registerPushEndpoint(
        "https://pm.example",
        "acc-1",
        "42",
        { token: "", environment: "sandbox", locale: "", previewsEnabled: false },
        fetchStub(400, { error: "token must be 1 to 512 characters" }),
      ),
    ).rejects.toMatchObject({ status: 400, message: "token must be 1 to 512 characters" });
  });
});

describe("bearerJsonFetch", () => {
  it("resolves daemon paths against the controller with the bearer header", async () => {
    const requests: Request[] = [];
    const res = await bearerJsonFetch("https://pm.example/", "acc-1", fetchStub(200, { ok: 1 }, requests))(
      "/api/forwards/7/token",
      { method: "POST" },
    );
    expect(requests[0].url).toBe("https://pm.example/api/forwards/7/token");
    expect(requests[0].method).toBe("POST");
    expect(requests[0].headers["Authorization"]).toBe("Bearer acc-1");
    expect(requests[0].url).not.toContain("acc-1");
    expect(res.ok).toBe(true);
    await expect(res.json()).resolves.toEqual({ ok: 1 });
  });

  it("raises a 401 as AuthHttpError so the session can rotate the token", async () => {
    const call = bearerJsonFetch("https://pm.example", "stale", fetchStub(401, { error: "expired" }))("/api/x", {
      method: "POST",
    });
    await expect(call).rejects.toMatchObject({ name: "AuthHttpError", status: 401, message: "expired" });
  });

  it("lets the shared forward helper mint and build the opening URL", async () => {
    const requests: Request[] = [];
    const url = await openableForwardUrl(
      bearerJsonFetch("https://pm.example", "acc-1", fetchStub(200, { token: "tok", expiresInMs: 5 }, requests)),
      "7",
      "https://pm.example/forwards/7/?a=1",
    );
    expect(requests[0].url).toBe("https://pm.example/api/forwards/7/token");
    expect(url).toBe("https://pm.example/forwards/7/?a=1&fwd_token=tok");
  });
});
