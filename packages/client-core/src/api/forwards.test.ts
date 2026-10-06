import { describe, expect, it } from "vitest";
import type { JsonFetch, JsonResponse } from "../platform";
import {
  ForwardTokenError,
  forwardTokenPath,
  forwardUrlWithToken,
  mintForwardToken,
  openableForwardUrl,
  parseForwardTokenResponse,
} from "./forwards";

function response(status: number, body: unknown): JsonResponse {
  return { ok: status >= 200 && status < 300, status, json: async () => body };
}

describe("forwardUrlWithToken", () => {
  it("starts a query string when the URL has none", () => {
    expect(forwardUrlWithToken("http://10.0.0.5:57932", "tok")).toBe("http://10.0.0.5:57932?fwd_token=tok");
  });

  it("appends to an existing query string", () => {
    expect(forwardUrlWithToken("http://host:1/app?x=1", "tok")).toBe("http://host:1/app?x=1&fwd_token=tok");
  });

  it("keeps the fragment after the token", () => {
    expect(forwardUrlWithToken("http://host:1/app?x=1#/route", "tok")).toBe(
      "http://host:1/app?x=1&fwd_token=tok#/route",
    );
    expect(forwardUrlWithToken("http://host:1/#/route", "tok")).toBe("http://host:1/?fwd_token=tok#/route");
  });

  it("escapes token characters that would break the query", () => {
    expect(forwardUrlWithToken("http://host:1", "a&b=c")).toBe("http://host:1?fwd_token=a%26b%3Dc");
  });
});

describe("parseForwardTokenResponse", () => {
  it("accepts the daemon envelope", () => {
    expect(parseForwardTokenResponse({ token: "t", expiresInMs: 60000 })).toEqual({ token: "t", expiresInMs: 60000 });
  });

  it("rejects malformed payloads", () => {
    expect(() => parseForwardTokenResponse(null)).toThrow("malformed");
    expect(() => parseForwardTokenResponse({ token: "", expiresInMs: 1 })).toThrow("malformed");
    expect(() => parseForwardTokenResponse({ token: "t" })).toThrow("malformed");
  });
});

describe("mintForwardToken", () => {
  it("posts to the forward's token endpoint", async () => {
    const calls: Array<{ path: string; method?: string }> = [];
    const fetchImpl: JsonFetch = async (path, options) => {
      calls.push({ path, method: options?.method });
      return response(200, { token: "tok", expiresInMs: 1000 });
    };
    await expect(mintForwardToken(fetchImpl, "42")).resolves.toEqual({ token: "tok", expiresInMs: 1000 });
    expect(calls).toEqual([{ path: forwardTokenPath("42"), method: "POST" }]);
    expect(forwardTokenPath("42")).toBe("/api/forwards/42/token");
  });

  it("reports 401 as not authenticated with the status attached", async () => {
    const fetchImpl: JsonFetch = async () => response(401, null);
    const err = await mintForwardToken(fetchImpl, "1").catch((e: unknown) => e);
    expect(err).toBeInstanceOf(ForwardTokenError);
    expect((err as ForwardTokenError).status).toBe(401);
    expect((err as ForwardTokenError).message).toBe("not authenticated");
  });

  it("prefers the daemon's error message on other failures", async () => {
    const fetchImpl: JsonFetch = async () => response(404, { error: "no such forward" });
    await expect(mintForwardToken(fetchImpl, "1")).rejects.toThrow("no such forward");
  });
});

describe("openableForwardUrl", () => {
  it("returns the forward URL carrying the minted token", async () => {
    const fetchImpl: JsonFetch = async () => response(200, { token: "tok", expiresInMs: 1000 });
    await expect(openableForwardUrl(fetchImpl, "7", "https://pm.example/prefix/forwards/1/?a=b")).resolves.toBe(
      "https://pm.example/prefix/forwards/1/?a=b&fwd_token=tok",
    );
  });
});
