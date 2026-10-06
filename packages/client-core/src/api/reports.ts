export type ReportKind = "checkpoint" | "user-note" | "status" | "progress" | "blocked";

export interface ActivityReport {
  tsUnixMs: number;
  kind: ReportKind;
  payload: unknown;
}

/** Sort reports newest-first by timestamp. */
export function sortNewestFirst(reports: readonly ActivityReport[]): ActivityReport[] {
  return [...reports].sort((a, b) => b.tsUnixMs - a.tsUnixMs);
}

function reportKey(r: ActivityReport): string {
  return `${r.tsUnixMs}:${r.kind}:${JSON.stringify(r.payload)}`;
}

/** Union a fresh fetch with what is held, dropping exact duplicates. */
export function mergeReports(
  existing: readonly ActivityReport[],
  incoming: readonly ActivityReport[],
): ActivityReport[] {
  const seen = new Map<string, ActivityReport>();
  for (const r of [...existing, ...incoming]) {
    seen.set(reportKey(r), r);
  }
  return sortNewestFirst([...seen.values()]);
}

/**
 * Parse the server's session-reports JSON envelope.
 *
 * The daemon returns `{ "reports": ActivityReport[] }`.  Both web and mobile
 * must go through this function so the contract is pinned in one place.
 */
export function parseReportsResponse(body: unknown): ActivityReport[] {
  const obj = body as { reports?: unknown };
  if (!obj || !Array.isArray(obj.reports)) return [];
  return obj.reports as ActivityReport[];
}
