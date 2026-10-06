/** Ordering, labels and wording for connection-call approvals; the controller decides and executes. */

/** A call as `/api/connection-approvals` lists it: no arguments or result. */
export interface ApprovalSummary {
  id: string;
  session_id: number;
  project_id: number;
  connection_id: number;
  connection_revision: number;
  tool: string;
  justification: string;
  status: string;
  created_at: number;
  expires_at: number;
  decided_by: string | null;
  error: string | null;
  has_result?: boolean;
  project_name: string | null;
  session_name: string | null;
  session_state: string | null;
  session_role: string | null;
  session_live: boolean;
  connection_name: string | null;
  connection_endpoint: string | null;
  connection_kind: string | null;
  connection_active: boolean;
  connection_current: boolean;
  tool_access?: string;
  tool_description?: string | null;
}

/** One approval as `/api/connection-approvals/{id}` returns it. */
export interface ApprovalDetail extends ApprovalSummary {
  request_id: string;
  arguments: unknown;
  result: unknown | null;
}

export type ApprovalFilter = "all" | "pending" | "decided";
export type ApprovalTone = "attention" | "good" | "bad" | "neutral" | "busy";

/** Newest request first; equal times keep a stable order by id. */
export function sortApprovals<T extends Pick<ApprovalSummary, "created_at" | "id">>(
  approvals: readonly T[],
): T[] {
  return [...approvals].sort(
    (a, b) => b.created_at - a.created_at || (a.id < b.id ? -1 : a.id > b.id ? 1 : 0),
  );
}

export function filterApprovals<T extends Pick<ApprovalSummary, "status">>(
  approvals: readonly T[],
  filter: ApprovalFilter,
): T[] {
  if (filter === "all") return [...approvals];
  return approvals.filter((approval) =>
    filter === "pending" ? approval.status === "pending" : approval.status !== "pending",
  );
}

export function pendingCount(approvals: readonly Pick<ApprovalSummary, "status">[]): number {
  return approvals.filter((approval) => approval.status === "pending").length;
}

const STATUS: Record<string, { label: string; tone: ApprovalTone }> = {
  pending: { label: "Waiting for you", tone: "attention" },
  authorized: { label: "Approved, queued", tone: "busy" },
  executing: { label: "Approved, running", tone: "busy" },
  succeeded: { label: "Approved, succeeded", tone: "good" },
  failed: { label: "Approved, failed", tone: "bad" },
  outcome_unknown: { label: "Approved, outcome unknown", tone: "bad" },
  denied: { label: "Denied", tone: "neutral" },
  expired: { label: "Expired", tone: "neutral" },
  canceled: { label: "Canceled", tone: "neutral" },
};

export function approvalStatus(status: string): { label: string; tone: ApprovalTone } {
  return STATUS[status] ?? { label: status, tone: "neutral" };
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/** "just now", "5 min ago", "3 h ago", "2 d ago". */
export function relativeTime(at: number, now: number): string {
  const elapsed = now - at;
  if (elapsed < MINUTE) return "just now";
  if (elapsed < HOUR) return `${Math.floor(elapsed / MINUTE)} min ago`;
  if (elapsed < DAY) return `${Math.floor(elapsed / HOUR)} h ago`;
  return `${Math.floor(elapsed / DAY)} d ago`;
}

/** How long a pending approval can still be decided, or null once it cannot. */
export function expiresIn(expiresAt: number, now: number): string | null {
  const left = expiresAt - now;
  if (left <= 0) return null;
  if (left < HOUR) return `${Math.max(1, Math.ceil(left / MINUTE))} min`;
  return `${Math.floor(left / HOUR)} h ${Math.floor((left % HOUR) / MINUTE)} min`;
}

/** An unambiguous local timestamp: date, time with seconds, and zone. */
export function absoluteTime(at: number, locale?: string): string {
  return new Date(at).toLocaleString(locale, {
    year: "numeric",
    month: "short",
    day: "numeric",
    hour: "2-digit",
    minute: "2-digit",
    second: "2-digit",
    timeZoneName: "short",
  });
}

/** Reasons the controller would refuse a pending call if it were approved now. */
export function approvalWarnings(
  approval: Pick<ApprovalSummary, "status" | "session_live" | "connection_active" | "connection_current">,
): string[] {
  if (approval.status !== "pending") return [];
  const warnings: string[] = [];
  if (!approval.connection_active) warnings.push("The connection is disabled, so an approval will fail.");
  else if (!approval.connection_current)
    warnings.push("The connection changed after this request, so an approval will fail.");
  if (!approval.session_live) warnings.push("The requesting session has ended, so an approval will fail.");
  return warnings;
}

/** What a decision achieved, judged from the status the controller returned, failures included. */
export function decisionOutcome(approve: boolean, status: string): { ok: boolean; message: string } {
  if (!approve)
    return status === "denied"
      ? { ok: true, message: "Denied. The tool was not invoked." }
      : { ok: false, message: `Not denied: this approval was already ${approvalStatus(status).label.toLowerCase()}.` };
  switch (status) {
    case "authorized":
    case "executing":
      return { ok: true, message: "Approved. Running once with the arguments shown." };
    case "succeeded":
      return { ok: true, message: "Approved. The call succeeded." };
    case "failed":
      return { ok: false, message: "Approved, but the call failed." };
    case "outcome_unknown":
      return { ok: false, message: "Approved, but the outcome is unknown. Check the service before retrying." };
    default:
      return { ok: false, message: `Not approved: this approval was already ${approvalStatus(status).label.toLowerCase()}.` };
  }
}

/** The arguments exactly as they will be sent, for review. */
export function formatArguments(value: unknown): string {
  return JSON.stringify(value ?? {}, null, 2);
}

/** Approvals that became pending since the last look, oldest first; the first look announces nothing. */
export function newlyPending<T extends Pick<ApprovalSummary, "id" | "status" | "created_at">>(
  seen: ReadonlySet<string> | null,
  approvals: readonly T[],
): { fresh: T[]; seen: Set<string> } {
  const pending = approvals.filter((approval) => approval.status === "pending");
  const next = new Set(pending.map((approval) => approval.id));
  if (seen === null) return { fresh: [], seen: next };
  const fresh = sortApprovals(pending.filter((approval) => !seen.has(approval.id))).reverse();
  return { fresh, seen: next };
}
