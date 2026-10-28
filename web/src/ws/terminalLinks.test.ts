import { create } from "@bufbuild/protobuf";
import { afterEach, describe, expect, it, vi } from "vitest";
import { ItemSchema, ProjectSchema, SessionSchema, TerminalSchema } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { initialState, itemKey, type AppState } from "@puppet-master/client-core/state/reducer";
import { isSafeHttpUrl, openTerminalHttpLink, resolveTerminalPmLink } from "./terminalLinks";

function state(): AppState {
  const largeId = 9_007_199_254_740_993n;
  return {
    ...initialState,
    hydrated: true,
    projects: new Map([
      ["10", create(ProjectSchema, { id: 10n, bucketId: 1n })],
      ["20", create(ProjectSchema, { id: 20n, bucketId: 2n })],
    ]),
    sessions: new Map([
      ["100", create(SessionSchema, { id: 100n, projectId: 10n })],
      [largeId.toString(), create(SessionSchema, { id: largeId, projectId: 10n })],
      ["200", create(SessionSchema, { id: 200n, projectId: 20n })],
    ]),
    terminals: new Map([
      ["1000", create(TerminalSchema, { id: 1000n, sessionId: 100n })],
    ]),
    items: new Map([
      [itemKey(1n, largeId), create(ItemSchema, { id: largeId, bucketId: 1n })],
      [itemKey(2n, 2n), create(ItemSchema, { id: 2n, bucketId: 2n })],
    ]),
  };
}

describe("terminal link destinations", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("routes same-bucket items and sessions without Number coercion", () => {
    const current = state();
    const largeId = "9007199254740993";
    expect(resolveTerminalPmLink(current, { kind: "session", id: "100" }, `pm:item/1/${largeId}`))
      .toEqual({ path: `/bucket/1/item/${largeId}` });
    expect(resolveTerminalPmLink(current, { kind: "terminal", id: "1000" }, `pm:session/${largeId}`))
      .toEqual({ path: `/session/${largeId}` });
  });

  it("rejects stale, cross-bucket, malformed, and unhydrated targets", () => {
    const current = state();
    expect(resolveTerminalPmLink(current, { kind: "session", id: "100" }, "pm:item/2/2").error)
      .toContain("outside this terminal's bucket");
    expect(resolveTerminalPmLink(current, { kind: "session", id: "100" }, "pm:item/2").error)
      .toContain("unqualified");
    expect(resolveTerminalPmLink(current, { kind: "session", id: "100" }, "pm:session/999").error)
      .toContain("missing or inaccessible");
    expect(resolveTerminalPmLink(current, { kind: "session", id: "100" }, "pm:item/nope").error)
      .toContain("Unsupported");
    expect(resolveTerminalPmLink(initialState, { kind: "session", id: "100" }, "pm:item/1/1").error)
      .toContain("still loading");
  });

  it("allows only well-formed HTTP(S) destinations", () => {
    expect(isSafeHttpUrl("http://localhost:3000/path")).toBe(true);
    expect(isSafeHttpUrl("https://localhost/%E2%9C%93")).toBe(true);
    expect(isSafeHttpUrl("javascript:alert(1)")).toBe(false);
    expect(isSafeHttpUrl("file:///tmp/a")).toBe(false);
    expect(isSafeHttpUrl("https://")).toBe(false);
    expect(isSafeHttpUrl("https://localhost/%zz")).toBe(false);
  });

  it("opens same-app review links without isolating the opener", () => {
    const currentLocation = new URL("http://localhost/forwards/12/");
    const open = vi.fn();
    vi.stubGlobal("location", currentLocation);
    vi.stubGlobal("window", { open });
    const href = `${currentLocation.href}#/review/4?file=src/file.ts&thread=2`;

    openTerminalHttpLink(href);

    expect(open).toHaveBeenCalledWith(href, "_blank");
  });

  it("isolates unrelated links from the opener", () => {
    const currentLocation = new URL("http://localhost/forwards/12/");
    const open = vi.fn();
    vi.stubGlobal("location", currentLocation);
    vi.stubGlobal("window", { open });
    const href = "http://127.0.0.1/forwards/12/#/session/4";

    openTerminalHttpLink(href);

    expect(open).toHaveBeenCalledWith(href, "_blank", "noopener,noreferrer");
  });
});
