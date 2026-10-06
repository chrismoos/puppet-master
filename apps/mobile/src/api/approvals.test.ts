import { afterEach, describe, expect, it, vi } from "vitest";

import { AuthHttpError } from "../auth/api";
import type { DeviceAuthSession } from "../auth/session";
import { decideApproval, fetchApproval, fetchApprovals } from "./approvals";

/** Retries once on an AuthHttpError 401, as DeviceAuthSession does. */
function auth(tokens = ["first", "second"]): DeviceAuthSession {
  return {
    withAccessToken: async <T>(_: string, call: (token: string) => Promise<T>) => {
      try {
        return await call(tokens[0]);
      } catch (error) {
        if (!(error instanceof AuthHttpError) || error.status !== 401) throw error;
        return await call(tokens[1]);
      }
    },
  } as unknown as DeviceAuthSession;
}

function respond(status: number, body: unknown) {
  return new Response(JSON.stringify(body), { status, headers: { "Content-Type": "application/json" } });
}

afterEach(() => vi.unstubAllGlobals());

describe("approvals API", () => {
  it("lists from the approvals endpoint with the device bearer", async () => {
    const fetch = vi.fn().mockResolvedValue(respond(200, []));
    vi.stubGlobal("fetch", fetch);
    await fetchApprovals(auth(), "https://pm.example/");
    expect(fetch.mock.calls[0][0]).toBe("https://pm.example/api/connection-approvals");
    expect(fetch.mock.calls[0][1].headers.Authorization).toBe("Bearer first");
  });

  it("decides through the shared decision endpoint, retrying a rejected token once", async () => {
    const fetch = vi
      .fn()
      .mockResolvedValueOnce(respond(401, {}))
      .mockResolvedValueOnce(respond(200, { status: "authorized" }));
    vi.stubGlobal("fetch", fetch);
    const call = await decideApproval(auth(), "https://pm.example", "abc", true);
    expect(call.status).toBe("authorized");
    expect(fetch).toHaveBeenCalledTimes(2);
    expect(fetch.mock.calls[1][0]).toBe("https://pm.example/api/connection-calls/abc/decision");
    expect(fetch.mock.calls[1][1]).toMatchObject({ method: "POST", body: '{"approve":true}' });
    expect(fetch.mock.calls[1][1].headers.Authorization).toBe("Bearer second");
  });

  it("encodes the id and surfaces the controller's error", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(respond(404, { error: "This approval no longer exists" })));
    await expect(fetchApproval(auth(), "https://pm.example", "a/b")).rejects.toThrow("no longer exists");
    expect(vi.mocked(fetch).mock.calls[0][0]).toBe("https://pm.example/api/connection-approvals/a%2Fb");
  });
});
