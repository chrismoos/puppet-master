import type { TerminalStatusActionKind, TerminalStatusView } from "../ws/terminalStatusView";

/** The React rendering of a terminal status, matching the stage's DOM rendering. */
export function TerminalStatus({ view, onAction }: {
  view: TerminalStatusView;
  onAction: (kind: TerminalStatusActionKind) => void;
}) {
  return (
    <div className={`terminal-stream-status is-${view.level}`} role={view.level === "blocked" ? "alert" : "status"}>
      <div className="terminal-status-box">
        {view.level === "wait" && <span className="terminal-status-spinner" aria-hidden="true" />}
        <div className="terminal-status-body">
          <div className="terminal-status-title">{view.title}</div>
          {view.detail && <div className="terminal-status-detail">{view.detail}</div>}
          {view.meta && <div className="terminal-status-meta">{view.meta}</div>}
        </div>
        {view.actions.length > 0 && (
          <div className="terminal-status-actions">
            {view.actions.map((action) => (
              <button
                key={action.kind}
                type="button"
                className={action.primary ? "is-primary" : undefined}
                onClick={() => onAction(action.kind)}
              >
                {action.label}
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
