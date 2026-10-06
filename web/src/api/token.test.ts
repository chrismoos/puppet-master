import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { accessToken, authedFetch, forgetAccessToken, UnauthenticatedError } from "./token";

const MINT = "/api/web/token";

function minted(token: string, ttlMs = 600_000): Response {
  return new Response(JSON.stringify({ accessToken: token, expiresAtUnixMs: Date.now() + ttlMs }), {
    status: 200,
  });
}

describe("the dashboard's access token", () => {
  beforeEach(() => forgetAccessToken());
  afterEach(() => vi.unstubAllGlobals());

  it("mints from the cookie and sends no credential of its own to do it", async () => {
    const fetchMock = vi.fn().mockResolvedValue(minted("first"));
    vi.stubGlobal("fetch", fetchMock);

    expect(await accessToken()).toBe("first");
    expect(fetchMock.mock.calls[0][0]).toBe(MINT);
    expect(fetchMock.mock.calls[0][1]).toEqual({ method: "POST" });
  });

  it("mints once for a burst, so no caller is left holding a superseded token", async () => {
    const fetchMock = vi.fn().mockResolvedValue(minted("shared"));
    vi.stubGlobal("fetch", fetchMock);

    const tokens = await Promise.all([accessToken(), accessToken(), accessToken()]);

    expect(tokens).toEqual(["shared", "shared", "shared"]);
    expect(fetchMock).toHaveBeenCalledTimes(1);
  });

  it("mints again rather than hand back a token about to expire", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValueOnce(minted("nearly-done", 1_000))
      .mockResolvedValueOnce(minted("fresh"));
    vi.stubGlobal("fetch", fetchMock);

    expect(await accessToken()).toBe("nearly-done");
    expect(await accessToken()).toBe("fresh");
  });

  it("attaches the token as a bearer header without disturbing the request", async () => {
    const fetchMock = vi.fn(async (path: string, _init?: RequestInit) =>
      path === MINT ? minted("bearer-me") : new Response("{}", { status: 200 }),
    );
    vi.stubGlobal("fetch", fetchMock);

    await authedFetch("/api/settings", { method: "PUT", headers: { "Content-Type": "text/plain" } });

    const [path, init] = fetchMock.mock.calls[1];
    expect(path).toBe("/api/settings");
    expect(init?.method).toBe("PUT");
    const headers = new Headers(init?.headers);
    expect(headers.get("Authorization")).toBe("Bearer bearer-me");
    expect(headers.get("Content-Type")).toBe("text/plain");
  });

  /// A restart drops the daemon's tokens, which is ordinary rather than a sign
  /// the user is signed out, so one refusal is retried with a fresh token.
  it("mints again and retries once when the controller refuses the token", async () => {
    let mints = 0;
    const fetchMock = vi.fn(async (path: string, _init?: RequestInit) => {
      if (path === MINT) {
        mints += 1;
        return minted(`token-${mints}`);
      }
      return new Response("", { status: mints === 1 ? 401 : 200 });
    });
    vi.stubGlobal("fetch", fetchMock);

    const res = await authedFetch("/api/me");

    expect(res.status).toBe(200);
    expect(mints).toBe(2);
    const retried = new Headers(fetchMock.mock.calls[3][1]?.headers);
    expect(retried.get("Authorization")).toBe("Bearer token-2");
  });

  it("does not retry a second refusal, so a signed-out page stops asking", async () => {
    const fetchMock = vi.fn(async (path: string, _init?: RequestInit) =>
      path === MINT ? minted("stale") : new Response("", { status: 401 }),
    );
    vi.stubGlobal("fetch", fetchMock);

    expect((await authedFetch("/api/me")).status).toBe(401);
    expect(fetchMock.mock.calls.filter(([path]) => path !== MINT)).toHaveLength(2);
  });

  it("reports a refused mint as being signed out rather than as a failure", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(new Response("", { status: 401 })),
    );

    await expect(accessToken()).rejects.toBeInstanceOf(UnauthenticatedError);
  });

  it("refuses a mint that answered without a token", async () => {
    vi.stubGlobal(
      "fetch",
      vi.fn().mockResolvedValue(new Response(JSON.stringify({ ok: true }), { status: 200 })),
    );

    await expect(accessToken()).rejects.toThrow(/minted no access token/);
  });
});
