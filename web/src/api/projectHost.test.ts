import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";

import { blocksSpawn, fetchProjectHost, type ProjectHostState } from "./projectHost";
import { seedAccessToken } from "./token.fixture";

// Every authenticated call mints a token when it holds none, which would
// otherwise be the first call a stub answers.
beforeEach(seedAccessToken);

function state(partial: Partial<ProjectHostState>): ProjectHostState {
  return { status: "ready", path: "/srv/acme", detail: "", ...partial };
}

describe("blocksSpawn", () => {
  it("lets a usable path through", () => {
    expect(blocksSpawn(state({ status: "ready" }))).toBe(false);
  });

  it("lets an unchecked path through, because nothing says it is broken", () => {
    expect(blocksSpawn(state({ status: "path-unchecked" }))).toBe(false);
  });

  it("stops every condition that would start a session nowhere useful", () => {
    for (const status of [
      "host-offline",
      "path-unset",
      "path-missing",
      "path-not-a-directory",
      "path-unreadable",
    ] as const) {
      expect(blocksSpawn(state({ status }))).toBe(true);
    }
  });

  it("does not block before the check has answered", () => {
    expect(blocksSpawn(null)).toBe(false);
  });
});

describe("fetchProjectHost", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("asks about one project on one host", async () => {
    const fetchMock = vi.fn().mockResolvedValue(
      new Response(
        JSON.stringify({
          status: "path-missing",
          path: "/srv/acme",
          detail: 'project "acme" path "/srv/acme" does not exist on host "lima" (id 4)',
        }),
        { status: 200 },
      ),
    );
    vi.stubGlobal("fetch", fetchMock);
    const answer = await fetchProjectHost(7n, 4n);
    expect(fetchMock.mock.calls[0][0]).toBe("/api/project-host?project=7&worker=4");
    expect(answer.status).toBe("path-missing");
    expect(answer.detail).not.toContain("offline");
  });

  it("surfaces the controller's reason for a failed check", async () => {
    vi.stubGlobal(
      "fetch",
      vi
        .fn()
        .mockResolvedValue(new Response(JSON.stringify({ error: "no such project" }), { status: 400 })),
    );
    await expect(fetchProjectHost(7n, 0n)).rejects.toThrow("no such project");
  });
});
