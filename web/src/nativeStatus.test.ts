import { describe, expect, it } from "vitest";
import {
  ATTACHING,
  CLOSE_INCOMPATIBLE,
  CLOSE_PAGE_OUTDATED,
  MANUAL_RETRY_DELAY_MS,
  STATUS_DELAY_MS,
  closedLink,
  nativeStreamStatus,
  onlineLink,
  reattachDelay,
} from "./nativeStatus";

describe("a native terminal's status", () => {
  it("says nothing for a quick attach, then waits, then offers a retry", () => {
    const link = ATTACHING(0);
    expect(nativeStreamStatus(link, null, STATUS_DELAY_MS - 1)).toBeNull();
    expect(nativeStreamStatus(link, null, STATUS_DELAY_MS)).toEqual({ phase: "reconnecting", canRetry: false });
    const lost = closedLink(link, "socket closed", 1006, 100);
    expect(nativeStreamStatus(lost, null, MANUAL_RETRY_DELAY_MS)).toEqual({
      phase: "reconnecting",
      canRetry: true,
      lastError: "socket closed",
      problem: "connection",
    });
    expect(nativeStreamStatus(onlineLink(), null, MANUAL_RETRY_DELAY_MS)).toBeNull();
  });

  it("names an offline worker before anything else", () => {
    expect(nativeStreamStatus(onlineLink(), "local worker is offline", 0)).toBeNull();
    expect(nativeStreamStatus(ATTACHING(0), "local worker is offline", 0)).toEqual({
      phase: "reconnecting",
      canRetry: false,
      lastError: "local worker is offline",
      problem: "worker",
    });
  });

  it("shows an engine mismatch at once and keeps the wait that began first", () => {
    const refused = closedLink(ATTACHING(0), "", CLOSE_INCOMPATIBLE, 5);
    expect(refused.waitingSince).toBe(0);
    expect(nativeStreamStatus(refused, null, 6)).toMatchObject({ problem: "incompatible", canRetry: true });
    expect(nativeStreamStatus(closedLink(ATTACHING(0), "", CLOSE_PAGE_OUTDATED, 5), null, 6)).toMatchObject({ problem: "page-outdated" });
  });

  it("backs off attaching again up to a ceiling", () => {
    expect([0, 1, 2, 3, 4, 10].map(reattachDelay)).toEqual([250, 500, 1000, 2000, 4000, 4000]);
  });
});
