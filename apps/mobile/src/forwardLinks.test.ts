import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { SessionForwardSchema } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import type { JsonFetch } from "@puppet-master/client-core/platform";
import { resolveTappedUrl, type ForwardLinkDeps } from "./forwardLinks";

const forwards = [create(SessionForwardSchema, { id: 7n, sessionId: 1n, url: "https://preview.share.pm.example/" })];

function deps(mint: JsonFetch, calls: string[] = []): ForwardLinkDeps {
  return {
    forwards: () => forwards,
    withFetch: (call) => call(async (path, options) => {
      calls.push(`${options?.method ?? "GET"} ${path}`);
      return mint(path, options);
    }),
  };
}

const minted: JsonFetch = async () =>
  ({ ok: true, status: 200, json: async () => ({ token: "tok", expiresInMs: 1000 }) }) as Awaited<ReturnType<JsonFetch>>;

describe("resolveTappedUrl", () => {
  it("signs a link into a known forward, validating the full destination", async () => {
    const calls: string[] = [];
    const url = "https://preview.share.pm.example/docs/a.html#intro";
    await expect(resolveTappedUrl(url, deps(minted, calls))).resolves.toBe(
      "https://preview.share.pm.example/docs/a.html?fwd_token=tok#intro",
    );
    expect(calls).toEqual([`POST /api/forwards/7/token?destination=${encodeURIComponent(url)}`]);
  });

  it("leaves other links alone without asking the controller", async () => {
    const calls: string[] = [];
    await expect(resolveTappedUrl("https://example.com/x", deps(minted, calls))).resolves.toBe("https://example.com/x");
    expect(calls).toEqual([]);
  });

  it("falls back to the plain link when no token can be minted", async () => {
    const refused: JsonFetch = async () => {
      throw new Error("destination outside the forward");
    };
    await expect(resolveTappedUrl("https://preview.share.pm.example/", deps(refused))).resolves.toBe(
      "https://preview.share.pm.example/",
    );
  });
});
