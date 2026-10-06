import { describe, expect, it } from "vitest";
import {
  approvalStatus,
  approvalWarnings,
  decisionOutcome,
  expiresIn,
  filterApprovals,
  formatArguments,
  newlyPending,
  pendingCount,
  relativeTime,
  sortApprovals,
} from "./approvals";

const at = (id: string, created_at: number, status = "pending") => ({ id, created_at, status });

describe("approval ordering", () => {
  it("lists the newest request first whatever order the server sent", () => {
    const sorted = sortApprovals([at("old", 1), at("new", 3), at("mid", 2)]);
    expect(sorted.map((approval) => approval.id)).toEqual(["new", "mid", "old"]);
  });

  it("keeps simultaneous requests in a stable order", () => {
    const sorted = sortApprovals([at("b", 5), at("a", 5)]);
    expect(sorted.map((approval) => approval.id)).toEqual(["a", "b"]);
  });

  it("does not reorder the caller's array", () => {
    const input = [at("old", 1), at("new", 2)];
    sortApprovals(input);
    expect(input[0].id).toBe("old");
  });

  it("filters pending from decided and counts what waits", () => {
    const all = [at("p", 1), at("d", 2, "denied"), at("s", 3, "succeeded")];
    expect(filterApprovals(all, "pending").map((a) => a.id)).toEqual(["p"]);
    expect(filterApprovals(all, "decided").map((a) => a.id)).toEqual(["d", "s"]);
    expect(filterApprovals(all, "all")).toHaveLength(3);
    expect(pendingCount(all)).toBe(1);
  });
});

describe("approval status", () => {
  it("names every state the controller records", () => {
    for (const status of ["pending", "authorized", "executing", "succeeded", "failed", "outcome_unknown", "denied", "expired", "canceled"]) {
      expect(approvalStatus(status).label).not.toBe(status);
    }
    expect(approvalStatus("pending").tone).toBe("attention");
    expect(approvalStatus("something-new")).toEqual({ label: "something-new", tone: "neutral" });
  });

  it("reports each outcome a decision can meet, failures included", () => {
    expect(decisionOutcome(true, "authorized")).toEqual({ ok: true, message: "Approved. Running once with the arguments shown." });
    expect(decisionOutcome(true, "executing").ok).toBe(true);
    expect(decisionOutcome(true, "succeeded")).toEqual({ ok: true, message: "Approved. The call succeeded." });
    expect(decisionOutcome(true, "failed")).toEqual({ ok: false, message: "Approved, but the call failed." });
    expect(decisionOutcome(true, "outcome_unknown").ok).toBe(false);
    expect(decisionOutcome(true, "outcome_unknown").message).toContain("unknown");
    expect(decisionOutcome(false, "denied")).toEqual({ ok: true, message: "Denied. The tool was not invoked." });
  });

  it("reports a decision that lost a race instead of claiming it", () => {
    expect(decisionOutcome(true, "denied")).toEqual({ ok: false, message: "Not approved: this approval was already denied." });
    expect(decisionOutcome(true, "expired").ok).toBe(false);
    expect(decisionOutcome(false, "succeeded")).toEqual({ ok: false, message: "Not denied: this approval was already approved, succeeded." });
  });

  it("warns before approving a call the controller will refuse", () => {
    const fine = { status: "pending", session_live: true, connection_active: true, connection_current: true };
    expect(approvalWarnings(fine)).toEqual([]);
    expect(approvalWarnings({ ...fine, connection_current: false })).toHaveLength(1);
    expect(approvalWarnings({ ...fine, session_live: false, connection_active: false })).toHaveLength(2);
    expect(approvalWarnings({ ...fine, status: "denied", session_live: false })).toEqual([]);
  });
});

describe("approval times", () => {
  const now = 10 * 24 * 3_600_000;
  it("reads elapsed time at a glance", () => {
    expect(relativeTime(now - 5_000, now)).toBe("just now");
    expect(relativeTime(now - 5 * 60_000, now)).toBe("5 min ago");
    expect(relativeTime(now - 3 * 3_600_000, now)).toBe("3 h ago");
    expect(relativeTime(now - 2 * 24 * 3_600_000, now)).toBe("2 d ago");
  });

  it("counts down to expiry and stops at it", () => {
    expect(expiresIn(now + 90 * 60_000, now)).toBe("1 h 30 min");
    expect(expiresIn(now + 30_000, now)).toBe("1 min");
    expect(expiresIn(now, now)).toBeNull();
  });

  it("shows arguments exactly, including an empty object", () => {
    expect(formatArguments({ body: { name: "x" } })).toBe('{\n  "body": {\n    "name": "x"\n  }\n}');
    expect(formatArguments(null)).toBe("{}");
  });
});

describe("approval alerts", () => {
  it("announces only approvals that became pending since the last look", () => {
    const first = newlyPending(null, [at("backlog", 1)]);
    expect(first.fresh).toEqual([]);
    const second = newlyPending(first.seen, [at("b", 3), at("backlog", 1), at("a", 2), at("done", 0, "denied")]);
    expect(second.fresh.map((a) => a.id)).toEqual(["a", "b"]);
    const third = newlyPending(second.seen, [at("b", 3, "succeeded"), at("a", 2)]);
    expect(third.fresh).toEqual([]);
    expect([...third.seen]).toEqual(["a"]);
  });
});
