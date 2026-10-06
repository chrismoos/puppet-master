import { describe, expect, it } from "vitest";
import { mergeReports, sortNewestFirst, type ActivityReport } from "./reports";

function report(tsUnixMs: number, kind: ActivityReport["kind"], payload: unknown): ActivityReport {
  return { tsUnixMs, kind, payload };
}

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
