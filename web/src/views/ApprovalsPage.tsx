import { useCallback, useEffect, useRef, useState } from "react";
import {
  absoluteTime,
  approvalStatus,
  approvalWarnings,
  decisionOutcome,
  expiresIn,
  filterApprovals,
  formatArguments,
  pendingCount,
  relativeTime,
  sortApprovals,
  type ApprovalDetail,
  type ApprovalFilter,
  type ApprovalSummary,
} from "@puppet-master/client-core/approvals";
import { approvalRoutePath } from "@puppet-master/client-core/router";
import { decideApproval, getApproval, listApprovals } from "../api/approvals";
import { navigate } from "../router";
import "./ApprovalsPage.css";

const REFRESH_MS = 5_000;
const FILTERS: Array<{ value: ApprovalFilter; label: string }> = [
  { value: "all", label: "All" },
  { value: "pending", label: "Waiting" },
  { value: "decided", label: "Decided" },
];
const FOCUSABLE = 'button:not([disabled]), a[href], [tabindex]:not([tabindex="-1"])';

function message(error: unknown, fallback: string): string {
  return error instanceof Error && error.message ? error.message : fallback;
}

function useNow(intervalMs = 30_000): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), intervalMs);
    return () => clearInterval(timer);
  }, [intervalMs]);
  return now;
}

function StatusBadge({ status }: { status: string }) {
  const { label, tone } = approvalStatus(status);
  return <span className={`approval-status is-${tone}`}>{label}</span>;
}

function Timestamp({ at, now }: { at: number; now: number }) {
  return (
    <time dateTime={new Date(at).toISOString()} title={absoluteTime(at)}>
      {relativeTime(at, now)}
    </time>
  );
}

/** The top bar's way in: beside the appearance switch, counting what waits. */
export function ApprovalsButton({ pending }: { pending: number }) {
  return (
    <a
      className="btn topbar-icon approvals-link"
      href={`#${approvalRoutePath()}`}
      title={pending ? `approvals: ${pending} waiting` : "approvals"}
      aria-label={pending ? `Approvals, ${pending} waiting` : "Approvals"}
    >
      <svg viewBox="0 0 16 16" aria-hidden="true">
        <path d="M8 1.5 2.5 3.5v4c0 3.3 2.3 5.9 5.5 7 3.2-1.1 5.5-3.7 5.5-7v-4z" />
        <path d="m5.5 8 1.8 1.8L10.8 6.3" />
      </svg>
      {pending > 0 && <span className="approvals-badge" aria-hidden="true">{pending > 99 ? "99+" : pending}</span>}
    </a>
  );
}

/** Approvals newest first, with the routed one open in a modal. */
export function ApprovalsPage({ id }: { id?: string }) {
  const [approvals, setApprovals] = useState<ApprovalSummary[] | null>(null);
  const [error, setError] = useState("");
  const [filter, setFilter] = useState<ApprovalFilter>("all");
  const now = useNow();
  const busy = useRef(false);

  const refresh = useCallback(async () => {
    if (busy.current) return;
    busy.current = true;
    try {
      setApprovals(sortApprovals(await listApprovals()));
      setError("");
    } catch (failure) {
      setError(message(failure, "Could not load approvals"));
    } finally {
      busy.current = false;
    }
  }, []);

  useEffect(() => {
    void refresh();
    const timer = setInterval(() => void refresh(), REFRESH_MS);
    return () => clearInterval(timer);
  }, [refresh]);

  const shown = approvals ? filterApprovals(approvals, filter) : [];
  const waiting = approvals ? pendingCount(approvals) : 0;

  return (
    <section className="approvals-page" aria-labelledby="approvals-title">
      <header className="approvals-head">
        <div>
          <h1 id="approvals-title">Approvals</h1>
          <p className="approvals-lede">
            Privileged connection calls agents asked you to decide, newest first.
            {approvals && (waiting ? ` ${waiting} waiting.` : " Nothing waiting.")}
          </p>
        </div>
        <div className="approvals-filters" role="radiogroup" aria-label="Show approvals">
          {FILTERS.map((option) => (
            <button
              key={option.value}
              type="button"
              role="radio"
              aria-checked={filter === option.value}
              className={filter === option.value ? "is-on" : ""}
              onClick={() => setFilter(option.value)}
            >
              {option.label}
            </button>
          ))}
        </div>
      </header>
      {error && (
        <div className="approvals-error" role="alert">
          {error}{" "}
          <button type="button" className="btn" onClick={() => void refresh()}>
            Retry
          </button>
        </div>
      )}
      {approvals === null && !error && (
        <p className="approvals-empty" role="status">Loading approvals…</p>
      )}
      {approvals !== null && shown.length === 0 && (
        <div className="approvals-empty" role="status">
          <p className="approvals-empty-title">
            {filter === "pending" ? "Nothing is waiting for you" : "No approvals yet"}
          </p>
          <p>
            When an agent calls a connection tool whose policy requires approval, it appears here
            and on your registered devices.
          </p>
        </div>
      )}
      {shown.length > 0 && (
        <ol className="approvals-list" aria-label="Approval requests, newest first">
          {shown.map((approval) => (
            <li key={approval.id}>
              <a
                href={`#${approvalRoutePath(approval.id)}`}
                className={`approval-row ${approval.id === id ? "is-selected" : ""}`}
                aria-current={approval.id === id ? "true" : undefined}
              >
                <code className="approval-tool">{approval.tool}</code>
                <span className="approval-row-context">
                  {approval.connection_name ?? `Connection ${approval.connection_id}`}
                  {" · "}
                  {approval.project_name ?? `Project ${approval.project_id}`}
                </span>
                <span className="approval-row-session">
                  {approval.session_name ?? `Session ${approval.session_id}`}
                </span>
                <Timestamp at={approval.created_at} now={now} />
                <StatusBadge status={approval.status} />
              </a>
            </li>
          ))}
        </ol>
      )}
      {id && (
        <ApprovalModal
          key={id}
          id={id}
          now={now}
          status={approvals?.find((approval) => approval.id === id)?.status}
          onDecided={() => void refresh()}
          onClose={() => navigate(approvalRoutePath())}
        />
      )}
    </section>
  );
}

export function ApprovalModal({
  id,
  now,
  status,
  onDecided,
  onClose,
}: {
  id: string;
  now: number;
  status?: string;
  onDecided: () => void;
  onClose: () => void;
}) {
  const [detail, setDetail] = useState<ApprovalDetail | null>(null);
  const [error, setError] = useState("");
  const [deciding, setDeciding] = useState<"approve" | "reject" | null>(null);
  const [outcome, setOutcome] = useState<{ ok: boolean; message: string } | null>(null);
  const dialogRef = useRef<HTMLElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);

  const load = useCallback(async () => {
    try {
      setDetail(await getApproval(id));
      setError("");
    } catch (failure) {
      setError(message(failure, "Could not load this approval"));
    }
  }, [id]);

  // Reload when the polled status moves, so a result arrives without a manual refresh.
  useEffect(() => {
    void load();
  }, [load, status]);

  useEffect(() => {
    const opener = document.activeElement as HTMLElement | null;
    closeRef.current?.focus();
    return () => opener?.focus?.();
  }, []);

  const onKeyDown = (event: React.KeyboardEvent) => {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
      return;
    }
    if (event.key !== "Tab" || !dialogRef.current) return;
    const focusable = [...dialogRef.current.querySelectorAll<HTMLElement>(FOCUSABLE)];
    if (focusable.length === 0) return;
    const first = focusable[0];
    const last = focusable[focusable.length - 1];
    if (event.shiftKey && document.activeElement === first) {
      event.preventDefault();
      last.focus();
    } else if (!event.shiftKey && document.activeElement === last) {
      event.preventDefault();
      first.focus();
    }
  };

  const decide = async (approve: boolean) => {
    setDeciding(approve ? "approve" : "reject");
    setOutcome(null);
    try {
      const call = await decideApproval(id, approve);
      setOutcome(decisionOutcome(approve, call.status));
    } catch (failure) {
      setOutcome({ ok: false, message: message(failure, "The decision was not recorded") });
    } finally {
      setDeciding(null);
      await load();
      onDecided();
    }
  };

  const pending = detail?.status === "pending";
  const left = detail && pending ? expiresIn(detail.expires_at, now) : null;
  return (
    <div
      className="modal-backdrop approval-modal-backdrop"
      onMouseDown={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <section
        ref={dialogRef}
        className="modal approval-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="approval-modal-title"
        onKeyDown={onKeyDown}
      >
        <header className="approval-modal-head">
          <div>
            <span className="approval-kicker">Connection call approval</span>
            <h2 id="approval-modal-title">
              <code>{detail?.tool ?? "Approval"}</code>
            </h2>
            {detail && <StatusBadge status={detail.status} />}
          </div>
          <button ref={closeRef} type="button" className="approval-modal-close" onClick={onClose} aria-label="Close approval">
            ×
          </button>
        </header>
        <div className="approval-modal-body">
          {!detail && error && (
            <div className="approvals-error" role="alert">
              {error}{" "}
              <button type="button" className="btn" onClick={() => void load()}>
                Retry
              </button>
            </div>
          )}
          {!detail && !error && <p className="approvals-empty" role="status">Loading approval…</p>}
          {detail && (
            <>
              {detail.justification && (
                <blockquote className="approval-justification">
                  <span className="approval-kicker">Agent's justification</span>
                  {detail.justification}
                </blockquote>
              )}
              <dl className="approval-facts">
                <dt>Requested</dt>
                <dd>
                  {absoluteTime(detail.created_at)}{" "}
                  <span className="muted">({relativeTime(detail.created_at, now)})</span>
                </dd>
                {pending && (
                  <>
                    <dt>Expires</dt>
                    <dd>
                      {left ? `in ${left}` : "now"} <span className="muted">({absoluteTime(detail.expires_at)})</span>
                    </dd>
                  </>
                )}
                {detail.decided_by && (
                  <>
                    <dt>Decided by</dt>
                    <dd>{detail.decided_by}</dd>
                  </>
                )}
                <dt>Project</dt>
                <dd>{detail.project_name ?? `Project ${detail.project_id}`}</dd>
                <dt>Session</dt>
                <dd>
                  <a href={`#/session/${detail.session_id}`}>{detail.session_name ?? `Session ${detail.session_id}`}</a>{" "}
                  <span className="muted">
                    #{detail.session_id}
                    {detail.session_role ? ` · ${detail.session_role}` : ""}
                    {detail.session_state ? ` · ${detail.session_state}` : ""}
                  </span>
                </dd>
                <dt>Connection</dt>
                <dd>
                  {detail.connection_name ?? `Connection ${detail.connection_id}`}{" "}
                  <span className="muted">
                    {detail.connection_kind === "mcp" ? "MCP · " : detail.connection_kind === "openapi" ? "REST · " : ""}
                    revision {detail.connection_revision}
                  </span>
                  {detail.connection_endpoint && <code className="approval-endpoint">{detail.connection_endpoint}</code>}
                </dd>
                <dt>Tool</dt>
                <dd>
                  <code>{detail.tool}</code>
                  {detail.tool_access && <span className="muted"> · classified {detail.tool_access}</span>}
                  {detail.tool_description && <span className="approval-tool-description">{detail.tool_description}</span>}
                </dd>
              </dl>
              <section className="approval-arguments" aria-label="Exact arguments">
                <h3>Exact arguments</h3>
                <p className="muted">Approving executes these arguments once, exactly as shown.</p>
                <pre>{formatArguments(detail.arguments)}</pre>
              </section>
              {approvalWarnings(detail).map((warning) => (
                <p key={warning} className="approvals-warning" role="note">
                  {warning}
                </p>
              ))}
              {outcome && (
                <p className={outcome.ok ? "approvals-outcome" : "approvals-error"} role={outcome.ok ? "status" : "alert"}>
                  {outcome.message}
                </p>
              )}
              {detail.error && (
                <p className="approvals-error" role="alert">
                  {detail.error}
                </p>
              )}
              {detail.result != null && (
                <details className="approval-result">
                  <summary>Result</summary>
                  <pre>{formatArguments(detail.result)}</pre>
                </details>
              )}
            </>
          )}
        </div>
        {pending && (
          <footer className="approval-modal-actions">
            <button type="button" className="btn" disabled={deciding !== null} onClick={() => void decide(false)}>
              {deciding === "reject" ? "Rejecting…" : "Reject"}
            </button>
            <button type="button" className="btn btn-primary" disabled={deciding !== null} onClick={() => void decide(true)}>
              {deciding === "approve" ? "Approving…" : "Approve and execute"}
            </button>
          </footer>
        )}
      </section>
    </div>
  );
}
