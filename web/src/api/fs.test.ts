import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import { fetchDirectory, filterEntries, listingPath, trailingSegment, type DirEntry } from "./fs";
import { seedAccessToken } from "./token.fixture";

// Every authenticated call mints a token when it holds none, which would
// otherwise be the first call a stub answers.
beforeEach(seedAccessToken);

function entries(...names: string[]): DirEntry[] {
  return names.map((name) => ({ name, path: `/home/${name}` }));
}

describe("trailingSegment", () => {
  it("returns the text after the last separator", () => {
    expect(trailingSegment("/home/testuser/pro")).toBe("pro");
    expect(trailingSegment("plain")).toBe("plain");
  });

  it("is empty when the input ends in a separator", () => {
    expect(trailingSegment("/home/testuser/")).toBe("");
  });
});

describe("filterEntries", () => {
  it("keeps entries whose name starts with the partial segment", () => {
    const all = entries("api", "app", "web");
    expect(filterEntries(all, "/home/ap").map((e) => e.name)).toEqual(["api", "app"]);
  });

  it("is case-insensitive", () => {
    expect(filterEntries(entries("API", "web"), "/home/ap").map((e) => e.name)).toEqual(["API"]);
  });

  it("returns everything when mid-directory (trailing separator)", () => {
    const all = entries("api", "web");
    expect(filterEntries(all, "/home/").map((e) => e.name)).toEqual(["api", "web"]);
  });
});

describe("listingPath", () => {
  it("lists the parent directory while a segment is being typed", () => {
    expect(listingPath("/home/testuser/pro")).toBe("/home/testuser");
  });

  it("lists the directory itself when it ends in a separator", () => {
    expect(listingPath("/home/testuser/")).toBe("/home/testuser/");
  });

  it("keeps the root when typing a top-level segment", () => {
    expect(listingPath("/ho")).toBe("/");
  });

  it("passes empty input through for the home default", () => {
    expect(listingPath("")).toBe("");
  });
});

describe("fetchDirectory", () => {
  afterEach(() => vi.unstubAllGlobals());

  function stubFetch() {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(
        new Response(JSON.stringify({ dir: "/", parent: null, entries: [] }), { status: 200 }),
      );
    vi.stubGlobal("fetch", fetchMock);
    return fetchMock;
  }

  it("omits the worker param for the local worker", async () => {
    const fetchMock = stubFetch();
    await fetchDirectory("/home/testuser", 0n);
    expect(fetchMock.mock.calls[0][0]).toBe("/api/fs?path=%2Fhome%2Ftestuser");
  });

  it("appends the worker id for a remote worker", async () => {
    const fetchMock = stubFetch();
    await fetchDirectory("/srv", 4n);
    expect(fetchMock.mock.calls[0][0]).toBe("/api/fs?path=%2Fsrv&worker=4");
  });
});
