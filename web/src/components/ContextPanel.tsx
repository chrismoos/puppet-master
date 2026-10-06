import { ContextKind, type ContextField, type SessionContext } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { classifyHref } from "@puppet-master/client-core/pmlink";
import { progressPercent, severityClass } from "./ContextChip";
import { Timeline } from "./Timeline";

function ContextValue({ field }: { field: ContextField }) {
  if (field.kind === ContextKind.URL) {
    // Through the same check the glance chip applies to this very field. The
    // value is agent-written and entity-decoded on the way in, so nothing
    // upstream guarantees it is a scheme a browser should follow, and the only
    // thing standing between it and an attribute was React's own
    // javascript:-specific sanitizing.
    if (classifyHref(field.value).kind !== "external") {
      return <span className="ctx-grid-value">{field.value}</span>;
    }
    return (
      <a className="ctx-grid-link" href={field.value} target="_blank" rel="noreferrer">
        {field.value} ↗
      </a>
    );
  }
  if (field.kind === ContextKind.CODE) {
    return <code className="ctx-grid-code">{field.value}</code>;
  }
  if (field.kind === ContextKind.PROGRESS) {
    const pct = progressPercent(field.value);
    if (pct === null) return <span className="ctx-grid-value">{field.value}</span>;
    return (
      <span className="ctx-grid-progress">
        <span className="ctx-bar">
          <span className="ctx-bar-fill" style={{ width: `${pct}%` }} />
        </span>
        <span className="ctx-grid-value">{Math.round(pct)}%</span>
      </span>
    );
  }
  if (field.kind === ContextKind.BADGE || field.kind === ContextKind.METRIC) {
    return <span className={`ctx-grid-badge ${severityClass(field.severity)}`}>{field.value}</span>;
  }
  return <span className="ctx-grid-value">{field.value}</span>;
}

/**
 * The full-height side panel: the agent's rolling summary, the detail
 * context as a typed grid, and the checkpoint/activity timeline.
 */
export function ContextPanel({
  context,
  sessionId,
  changeToken,
  summary,
  activity,
}: {
  context: SessionContext | undefined;
  sessionId: string;
  changeToken: string;
  summary: string;
  activity: string;
}) {
  const detail = context?.detail ?? [];
  const hasActivity = activity.trim().length > 0;
  return (
    <aside className="context-panel" aria-label="session context">
      {summary && <p className="ctx-summary">{summary}</p>}
      {!hasActivity && detail.length === 0 ? (
        <p className="muted-line ctx-empty">no context reported yet</p>
      ) : (
        <dl className="ctx-grid">
          {hasActivity && (
            <div className="ctx-grid-row is-activity">
              <dt className="ctx-grid-label">activity</dt>
              <dd className="ctx-grid-cell">
                <span className="ctx-grid-value">{activity}</span>
              </dd>
            </div>
          )}
          {detail.map((f) => (
            <div className="ctx-grid-row" key={f.key}>
              <dt className="ctx-grid-label">{f.label || f.key}</dt>
              <dd className="ctx-grid-cell">
                <ContextValue field={f} />
              </dd>
            </div>
          ))}
        </dl>
      )}
      <Timeline sessionId={sessionId} changeToken={changeToken} />
    </aside>
  );
}
