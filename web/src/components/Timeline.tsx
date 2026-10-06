import { useEffect, useState } from "react";
import { fetchReports, mergeReports, type ActivityReport } from "../api/reports";
import { unescapeHtml } from "@puppet-master/client-core/format";

function describe(report: ActivityReport): string {
  const p = report.payload as Record<string, unknown> | null;
  if (!p) return "";
  switch (report.kind) {
    case "checkpoint":
    case "user-note": {
      const note = typeof p.note === "string" ? unescapeHtml(p.note) : "";
      const headline = typeof p.headline === "string" ? unescapeHtml(p.headline) : "";
      return note && headline ? `${headline} — ${note}` : note || headline;
    }
    case "status": {
      const parts = [p.phase, p.detail].filter((s) => typeof s === "string" && s).join(" — ");
      return typeof p.task === "string" && p.task ? `${p.task}: ${parts}` : parts;
    }
    case "progress": {
      const pct = typeof p.percent === "number" ? `${p.percent}% ` : "";
      return `${pct}${typeof p.summary === "string" ? p.summary : ""}`.trim();
    }
    case "blocked":
      return typeof p.question === "string" ? unescapeHtml(p.question) : "";
    default:
      return "";
  }
}

export function Timeline({ sessionId, changeToken }: { sessionId: string; changeToken: string }) {
  const [reports, setReports] = useState<ActivityReport[]>([]);
  const [open, setOpen] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const bodyId = `session-${sessionId}-activity`;

  useEffect(() => {
    setReports([]);
    setError(null);
  }, [sessionId]);

  useEffect(() => {
    const controller = new AbortController();
    fetchReports(sessionId, controller.signal)
      .then((fresh) => setReports((held) => mergeReports(held, fresh)))
      .catch((err: unknown) => {
        if (!controller.signal.aborted) {
          setError(err instanceof Error ? err.message : String(err));
        }
      });
    return () => controller.abort();
    // changeToken advances on each SessionChanged event so the log stays current.
  }, [sessionId, changeToken]);

  return (
    <section className="timeline">
      <button
        type="button"
        className="timeline-toggle"
        aria-expanded={open}
        aria-controls={bodyId}
        onClick={() => setOpen((v) => !v)}
      >
        <span className="timeline-caret" aria-hidden="true">
          {open ? "▾" : "▸"}
        </span>
        <span className="sentence">activity</span>
        <span className="timeline-count">{reports.length}</span>
      </button>
      {open && (
        <div className="timeline-body" id={bodyId}>
          {error && <p className="timeline-error">{error}</p>}
          {!error && reports.length === 0 && <p className="muted-line">no reports yet</p>}
          <ol className="timeline-list">
            {reports.map((report, i) => (
              <li className="timeline-item" key={`${report.tsUnixMs}-${report.kind}-${i}`}>
                <div className="timeline-meta">
                  <span className={`timeline-kind tl-${report.kind}`}>{report.kind}</span>
                  <time className="timeline-time" dateTime={new Date(report.tsUnixMs).toISOString()}>
                    {new Date(report.tsUnixMs).toLocaleTimeString()}
                  </time>
                </div>
                <span className="timeline-text">{describe(report)}</span>
              </li>
            ))}
          </ol>
        </div>
      )}
    </section>
  );
}
