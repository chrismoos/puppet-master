import { describe, expect, it } from "vitest";
import type { JsonFetch } from "@puppet-master/client-core/platform";
import { openForward, parseForwardHandoff, type OpenedWindow } from "./forwardLink";

function fakeWindow() {
  const state = { navigated: null as string | null, closed: false };
  const win: OpenedWindow = {
    navigate: (url) => {
      state.navigated = url;
    },
    close: () => {
      state.closed = true;
    },
  };
  return { win, state };
}

const minting: JsonFetch = async () => ({ ok: true, status: 200, json: async () => ({ token: "tok", expiresInMs: 1 }) });
const unauthorized: JsonFetch = async () => ({ ok: false, status: 401, json: async () => null });

describe("openForward", () => {
  it("opens the tab first and navigates it to the tokened URL", async () => {
    const { win, state } = fakeWindow();
    let openedBeforeMint = false;
    const fetchImpl: JsonFetch = (path, options) => {
      openedBeforeMint = true;
      return minting(path, options);
    };
    await openForward(fetchImpl, () => win, "3", "http://10.0.0.5:57932/?a=1");
    expect(openedBeforeMint).toBe(true);
    expect(state.navigated).toBe("http://10.0.0.5:57932/?a=1&fwd_token=tok");
    expect(state.closed).toBe(false);
  });

  it("closes the tab and rethrows when the mint fails", async () => {
    const { win, state } = fakeWindow();
    await expect(openForward(unauthorized, () => win, "3", "http://host:1")).rejects.toThrow("not authenticated");
    expect(state.navigated).toBeNull();
    expect(state.closed).toBe(true);
  });

  it("fails without minting when the browser blocks the tab", async () => {
    let minted = false;
    const fetchImpl: JsonFetch = (path, options) => {
      minted = true;
      return minting(path, options);
    };
    await expect(openForward(fetchImpl, () => null, "3", "http://host:1")).rejects.toThrow("blocked");
    expect(minted).toBe(false);
  });
});

describe("forward handoff", () => {
  it("preserves the encoded path and query through login", async () => {
    const destination = "http://host:1/deep/page?a=one%20two&b=3";
    const hash = `#/forward-open?id=3&destination=${encodeURIComponent(destination)}`;
    expect(parseForwardHandoff(hash)).toEqual({ id: "3", destination });
    const { win, state } = fakeWindow();
    let mintPath = "";
    await openForward(async (path, options) => {
      mintPath = path;
      return minting(path, options);
    }, () => win, "3", destination, true);
    expect(mintPath).toBe(`/api/forwards/3/token?destination=${encodeURIComponent(destination)}`);
    expect(state.navigated).toBe(`${destination}&fwd_token=tok`);
  });
  it("rejects malformed handoffs and ignores other routes", () => {
    expect(parseForwardHandoff("#/sessions/3")).toBeNull();
    expect(() => parseForwardHandoff("#/forward-open?id=bad&destination=x")).toThrow("Invalid");
    expect(() => parseForwardHandoff("#/forward-open?id=3")).toThrow("Invalid");
  });
});
