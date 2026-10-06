import { describe, expect, it } from "vitest";
import {
  mergeReports,
  parseReportsResponse,
  sortNewestFirst,
  type ActivityReport,
} from "./reports";

function report(tsUnixMs: number, kind: ActivityReport["kind"], payload: unknown): ActivityReport {
  return { tsUnixMs, kind, payload };
}

// ---------------------------------------------------------------------------
// parseReportsResponse — pinned to the daemon's JSON envelope
// ---------------------------------------------------------------------------

describe("parseReportsResponse", () => {
  it("extracts reports from the server envelope", () => {
    // The daemon returns: Json(serde_json::json!({ "reports": items }))
    const serverBody = {
      reports: [
        { tsUnixMs: 100, kind: "status", payload: { phase: "a" } },
        { tsUnixMs: 200, kind: "checkpoint", payload: { headline: "done" } },
      ],
    };
    const parsed = parseReportsResponse(serverBody);
    expect(parsed).toHaveLength(2);
    expect(parsed[0].tsUnixMs).toBe(100);
    expect(parsed[1].kind).toBe("checkpoint");
  });

  it("returns empty array when body is not an object", () => {
    expect(parseReportsResponse(null)).toEqual([]);
    expect(parseReportsResponse(undefined)).toEqual([]);
    expect(parseReportsResponse("string")).toEqual([]);
  });

  it("returns empty array when reports key is missing", () => {
    expect(parseReportsResponse({})).toEqual([]);
    expect(parseReportsResponse({ data: [] })).toEqual([]);
  });

  it("returns empty array when reports is not an array", () => {
    expect(parseReportsResponse({ reports: "not-array" })).toEqual([]);
    expect(parseReportsResponse({ reports: 42 })).toEqual([]);
  });

  it("rejects the bare-array shape that caused the mobile crash", () => {
    // Casting res.json() as ActivityReport[] would yield an object, not an
    // array, because the server wraps in { reports: [...] }.  Calling .sort()
    // on that object throws "data.sort is not a function".
    const bareArray = [{ tsUnixMs: 1, kind: "status", payload: {} }];
    // A bare array has no .reports property → falls back to empty.
    expect(parseReportsResponse(bareArray)).toEqual([]);
  });
});

// ---------------------------------------------------------------------------
// sortNewestFirst
// ---------------------------------------------------------------------------

describe("sortNewestFirst", () => {
  it("orders reports by descending timestamp", () => {
    const sorted = sortNewestFirst([
      report(100, "status", { phase: "a" }),
      report(300, "progress", { percent: 40 }),
      report(200, "blocked", { question: "?" }),
    ]);
    expect(sorted.map((r) => r.tsUnixMs)).toEqual([300, 200, 100]);
  });
});

// ---------------------------------------------------------------------------
// mergeReports
// ---------------------------------------------------------------------------

describe("mergeReports", () => {
  it("appends new reports and keeps newest first", () => {
    const held = [report(100, "status", { phase: "a" })];
    const fetched = [
      report(100, "status", { phase: "a" }),
      report(250, "progress", { percent: 60 }),
    ];
    const merged = mergeReports(held, fetched);
    expect(merged.map((r) => r.tsUnixMs)).toEqual([250, 100]);
  });

  it("drops exact duplicates across a refetch", () => {
    const same = report(100, "status", { phase: "a" });
    const merged = mergeReports([same], [same, same]);
    expect(merged).toHaveLength(1);
  });

  it("keeps distinct reports that share a timestamp", () => {
    const merged = mergeReports(
      [report(100, "status", { phase: "a" })],
      [report(100, "progress", { percent: 10 })],
    );
    expect(merged).toHaveLength(2);
  });
});
