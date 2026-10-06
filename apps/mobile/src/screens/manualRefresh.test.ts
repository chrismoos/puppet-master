import { describe, expect, it } from "vitest";

import {
  beginRefresh,
  endRefresh,
  idleRefresh,
  isRefreshing,
  type ManualRefreshState,
} from "./manualRefresh";

function begun(state: ManualRefreshState) {
  const result = beginRefresh(state);
  if (!result) throw new Error("expected the refresh to start");
  return result;
}

describe("manual refresh state", () => {
  it("is not refreshing until a pull starts one", () => {
    expect(isRefreshing(idleRefresh)).toBe(false);
    expect(isRefreshing(begun(idleRefresh).state)).toBe(true);
  });

  it("stops refreshing when the request succeeds", () => {
    const { state, token } = begun(idleRefresh);
    expect(isRefreshing(endRefresh(state, token))).toBe(false);
  });

  it("stops refreshing when the request fails", () => {
    const { state, token } = begun(idleRefresh);
    // The screen ends the refresh on both settlements, so failure takes the
    // same path: an unreachable controller must not spin forever.
    expect(isRefreshing(endRefresh(state, token))).toBe(false);
  });

  it("ignores a second pull while a request is outstanding", () => {
    const { state } = begun(idleRefresh);
    expect(beginRefresh(state)).toBeNull();
  });

  it("does not let a superseded answer clear a newer spinner", () => {
    const first = begun(idleRefresh);
    const idle = endRefresh(first.state, first.token);
    const second = begun(idle);
    expect(isRefreshing(endRefresh(second.state, first.token))).toBe(true);
    expect(isRefreshing(endRefresh(second.state, second.token))).toBe(false);
  });

  it("ignores an answer that arrives after the refresh already ended", () => {
    const { state, token } = begun(idleRefresh);
    const ended = endRefresh(state, token);
    expect(endRefresh(ended, token)).toBe(ended);
  });

  it("hands out a distinct token per refresh", () => {
    const first = begun(idleRefresh);
    const second = begun(endRefresh(first.state, first.token));
    expect(second.token).not.toBe(first.token);
  });
});
