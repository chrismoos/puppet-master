import { useEffect, useId, useRef, useState, type KeyboardEvent } from "react";
import { createPortal } from "react-dom";
import { LOCAL_WORKER_ID, sessionDisplayName } from "@puppet-master/client-core/format";
import { ModelProfileSource } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import type {
  AgentDialects,
  ModelProfile,
  Project,
  Session,
  Worker,
} from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { sessionLaunchMetadata } from "@puppet-master/client-core/state/sessionLaunch";
import { dialectLabel, selectEndpoint } from "@puppet-master/client-core/state/modelProfile";
import { copyTextToClipboard } from "../clipboard";

const FOCUSABLE =
  'button:not([disabled]), a[href], input:not([disabled]), select:not([disabled]), textarea:not([disabled]), [tabindex]:not([tabindex="-1"])';

function profileSourceLabel(source: ModelProfileSource): string {
  switch (source) {
    case ModelProfileSource.EXPLICIT:
      return "this spawn";
    case ModelProfileSource.PROJECT:
      return "the project";
    case ModelProfileSource.BUCKET:
      return "the bucket";
    default:
      return "spawn-time resolution";
  }
}

function CopyButton({ label, value }: { label: string; value: string }) {
  const [result, setResult] = useState<string | null>(null);

  const copy = async () => {
    try {
      await copyTextToClipboard(value);
      setResult("Copied");
    } catch {
      setResult("Copy unavailable");
    }
  };

  return (
    <span className="session-info-copy-wrap">
      <button type="button" className="session-info-copy" onClick={() => void copy()}>
        Copy {label}
      </button>
      {result && <span className="sr-only" role="status">{result}</span>}
    </span>
  );
}

function InfoValue({
  label,
  value,
  detail,
  code = false,
  copy = false,
}: {
  label: string;
  value: string;
  detail?: string;
  code?: boolean;
  copy?: boolean;
}) {
  return (
    <div className="session-info-field">
      <dt>{label}</dt>
      <dd>
        <span className={code ? "session-info-code" : undefined}>{value}</span>
        {copy && <CopyButton label={label.toLocaleLowerCase()} value={value} />}
        {detail && <small>{detail}</small>}
      </dd>
    </div>
  );
}

export function SessionInfoDialog({
  session,
  project,
  worker,
  modelProfile,
  agentDialects = [],
  returnFocus,
  onClose,
}: {
  session: Session;
  project?: Project;
  worker?: Worker;
  /** The profile the session recorded, looked up live so edits show. */
  modelProfile?: ModelProfile;
  agentDialects?: readonly AgentDialects[];
  returnFocus: HTMLElement | null;
  onClose: () => void;
}) {
  const titleId = useId();
  const noteId = useId();
  const dialogRef = useRef<HTMLElement>(null);
  const closeRef = useRef<HTMLButtonElement>(null);
  const launch = sessionLaunchMetadata(session, project);
  const local = session.workerId === LOCAL_WORKER_ID;
  const workerId = session.workerId.toString();

  useEffect(() => {
    closeRef.current?.focus();
    return () => returnFocus?.focus();
  }, [returnFocus]);

  const handleKeyDown = (event: KeyboardEvent<HTMLElement>) => {
    if (event.key === "Escape") {
      event.preventDefault();
      onClose();
      return;
    }
    if (event.key !== "Tab") return;
    const focusable = [...(dialogRef.current?.querySelectorAll<HTMLElement>(FOCUSABLE) ?? [])]
      .filter((element) => element.getClientRects().length > 0);
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

  const projectValue = project ? project.name : `Project ${session.projectId.toString()} unavailable`;
  const workerValue = local
    ? worker?.name || "Local worker"
    : worker?.name || `Worker ${workerId} unavailable`;
  const workerDetail = local
    ? "Runs on the controller host."
    : worker
      ? `${worker.online ? "Connected" : "Offline"} remote worker.`
      : "This worker is no longer registered or is not present in the current snapshot.";
  const hostValue = worker?.hostname || (local ? "Controller host" : "Not reported");
  // A session records the profile id, so a resume reselects the entry;
  // what shows here is what the next resume would use.
  const endpoint = selectEndpoint(modelProfile, session.agent, agentDialects);
  const profileValue = session.modelProfileId === undefined
    ? "Agent account"
    : modelProfile?.name ?? `Profile ${session.modelProfileId} deleted`;
  const profileDetail = session.modelProfileId === undefined
    ? "No model profile; the agent runs on its own account and default model."
    : `Resolved from ${profileSourceLabel(session.modelProfileSource)}.`;

  const dialog = (
    <div className="modal-backdrop session-info-backdrop" onMouseDown={(event) => {
      if (event.target === event.currentTarget) onClose();
    }}>
      <section
        ref={dialogRef}
        className="modal session-info-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby={titleId}
        aria-describedby={noteId}
        onKeyDown={handleKeyDown}
      >
        <header className="session-info-header">
          <div>
            <span className="session-info-eyebrow">session information</span>
            <h2 id={titleId}>{sessionDisplayName(session)}</h2>
          </div>
          <button ref={closeRef} type="button" className="session-info-close" onClick={onClose} aria-label="Close session information">×</button>
        </header>

        <div className="session-info-scroll">
          <section aria-labelledby={`${titleId}-launch`}>
            <h3 id={`${titleId}-launch`}>Launch</h3>
            <dl className="session-info-list">
              <InfoValue label="Origin" value={local ? "Local" : "Remote"} detail={local ? "Started on this Puppet Master controller." : "Started on a registered worker."} />
              <InfoValue
                label="Launch folder"
                value={launch.path ?? "Not recorded"}
                detail={launch.pathContext}
                code={launch.path !== null}
                copy={launch.path !== null}
              />
              <InfoValue label="Configured project" value={projectValue} detail={project ? undefined : "The project may have been deleted since launch."} />
              {project?.path && <InfoValue label="Project path" value={project.path} code copy />}
            </dl>
            <p className="session-info-note" id={noteId}>
              The launch folder is recorded when the session starts. It is not a live filesystem or git-status signal, and the path may have moved or been deleted since then.
            </p>
          </section>

          {session.git && (
            <section aria-labelledby={`${titleId}-git`}>
              <h3 id={`${titleId}-git`}>Git</h3>
              <dl className="session-info-list">
                {session.git.branch && (
                  <InfoValue label="Branch" value={session.git.branch} code copy />
                )}
                {session.git.worktree && (
                  <InfoValue
                    label="Worktree"
                    value={session.git.worktree}
                    detail={
                      session.git.repoRoot && session.git.repoRoot !== session.git.worktree
                        ? `Linked worktree of ${session.git.repoRoot}.`
                        : undefined
                    }
                    code
                    copy
                  />
                )}
                {session.git.repoRoot && session.git.repoRoot !== session.git.worktree && (
                  <InfoValue label="Repository" value={session.git.repoRoot} code copy />
                )}
                {session.git.commit && (
                  <InfoValue label="Commit" value={session.git.commit} code copy />
                )}
                {session.git.upstream && (
                  <InfoValue label="Upstream" value={session.git.upstream} code />
                )}
                {session.git.dirty !== undefined && (
                  <InfoValue
                    label="Working tree"
                    value={session.git.dirty ? "Uncommitted changes" : "Clean"}
                  />
                )}
              </dl>
              <p className="session-info-note">
                Reported by the agent, not read from disk. It is as current as the agent's last report.
              </p>
            </section>
          )}

          <section aria-labelledby={`${titleId}-runtime`}>
            <h3 id={`${titleId}-runtime`}>Worker and host</h3>
            <dl className="session-info-list">
              <InfoValue label="Worker" value={workerValue} detail={workerDetail} />
              <InfoValue label="Hostname" value={hostValue} />
              <InfoValue label="Worker ID" value={workerId} code copy />
            </dl>
          </section>

          <section aria-labelledby={`${titleId}-model`}>
            <h3 id={`${titleId}-model`}>Model</h3>
            <dl className="session-info-list">
              <InfoValue label="Model profile" value={profileValue} detail={profileDetail} />
              {endpoint && <InfoValue label="Model" value={endpoint.model} code />}
              {endpoint && (
                <InfoValue
                  label="Endpoint"
                  value={endpoint.baseUrl || "Agent default endpoint"}
                  detail={`${dialectLabel(endpoint.dialect)}${endpoint.backgroundModel ? ` · background ${endpoint.backgroundModel}` : ""}`}
                  code={endpoint.baseUrl !== ""}
                />
              )}
              {modelProfile && !endpoint && (
                <InfoValue
                  label="Endpoint"
                  value="None for this agent"
                  detail="This profile has no endpoint this session's agent can use, so a resume would be rejected."
                />
              )}
            </dl>
          </section>

          <section aria-labelledby={`${titleId}-identity`}>
            <h3 id={`${titleId}-identity`}>Identity</h3>
            <dl className="session-info-list">
              <InfoValue label="Session ID" value={session.id.toString()} code copy />
            </dl>
          </section>
        </div>

        <footer className="session-info-actions">
          <button type="button" className="btn" onClick={onClose}>Close</button>
        </footer>
      </section>
    </div>
  );

  // Keep the dialog out of transformed/clipped sidebar and Board-drawer ancestors.
  return typeof document === "undefined" ? dialog : createPortal(dialog, document.body);
}
