import { type MouseEvent, useCallback, useEffect, useMemo, useRef, useState } from "react";
import { ContextPanel } from "../components/ContextPanel";
import { sessionRoutePath } from "@puppet-master/client-core/router";
import { navigate } from "../router";
import { ReviewPage } from "./ReviewPage";
import { PlanWorkspace } from "./PlanWorkspace";
import { StateBadge } from "../components/StateBadge";
import { UnavailableWorkerChip, WorkerChip } from "../components/WorkerChip";
import { LOCAL_WORKER_ID, agentLabel, sessionDisplayName, sessionElapsed, sessionEnded, stateStyle, unescapeHtml } from "@puppet-master/client-core/format";
import { useAppState, useClient, useNow } from "../state/hooks";
import { workerUnavailableReason } from "@puppet-master/client-core/state/worker";
import { forwardsForSession, forwardsNewestFirst, visibleForwards } from "@puppet-master/client-core/state/forwards";
import { debugChips, type TerminalLayerDebug } from "../ws/terminalDiagnostics";
import type { TerminalStage } from "../ws/terminal";
import { PlanState, TerminalKind, type Session, type SessionForward } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { shellTabLabel } from "@puppet-master/client-core/terminal/label";
import { SessionKillDialog } from "./SessionKillDialog";
import { browserJsonFetch } from "../api/http";
import { browserWindowOpener, openForward } from "./forwardLink";
import { REVIEW_TAB_PREFIX, opensOwnWindow } from "./reviewWindow";

/// Distinguishes a plan tab from a terminal id in the same selection.
const PLAN_TAB_PREFIX = "plan:";

/// The middle mouse button in a MouseEvent. The right button is left to the
/// browser so the context menu can copy the link like any other.
const MIDDLE_BUTTON = 1;

export function forwardName(forward: Pick<SessionForward, "label" | "sourcePath" | "workerPort">): string {
  return forward.label || forward.sourcePath || `port ${forward.workerPort}`;
}

/// What a review's tab says. A range makes a poor name when several
/// are open, so an agent is asked for one; this only keeps it short
/// enough to sit in a tab.
export function reviewTabLabel(label: string, max = 22): string {
  const trimmed = label.trim();
  if (!trimmed) return "review";
  return trimmed.length <= max ? trimmed : `${trimmed.slice(0, max - 1)}…`;
}

export function reviewTabId(reviewId: bigint | number | string): string {
  return `${REVIEW_TAB_PREFIX}${reviewId}`;
}

/// The review a selection names, or null when it names a terminal.
export function selectedReviewId(selection: string): string | null {
  return selection.startsWith(REVIEW_TAB_PREFIX)
    ? selection.slice(REVIEW_TAB_PREFIX.length)
    : null;
}

export function planTabId(planId: bigint | number | string): string {
  return `${PLAN_TAB_PREFIX}${planId}`;
}

export function selectedPlanId(selection: string): string | null {
  return selection.startsWith(PLAN_TAB_PREFIX) ? selection.slice(PLAN_TAB_PREFIX.length) : null;
}

/// A tab's address written as a document URL rather than a bare
/// fragment, so opening it in a new window loads the app there instead
/// of the browser reading the click as a jump inside this one.
///
/// The base comes from where the app is actually served, which is not
/// always the site root, and never carries an origin so a published
/// forward keeps working through its prefix.
export function sessionTabHref(
  base: { pathname: string; search: string },
  sessionId: string,
  tab: string,
): string {
  return `${base.pathname}${base.search}#${sessionRoutePath(sessionId, tab, true)}`;
}

/// Whether the pane stays focused once the reader picks another tab.
///
/// A review or plan tab is only ever arrived at by address, and the
/// route forces focus mode for it, so the one transition left to decide
/// is leaving one: the reader asked for a terminal, which wants the
/// shell back rather than the room a review needed.
export function focusModeAfterLeaving(currentTab: string, currentFocus: boolean): boolean {
  if (currentTab.startsWith(REVIEW_TAB_PREFIX) || currentTab.startsWith(PLAN_TAB_PREFIX)) {
    return false;
  }
  return currentFocus;
}

/// Whether a review holds anything this reader has not had in front of
/// them. A count needs a rule behind it that nobody can guess; a mark
/// says the one thing that matters — there is something in here you
/// have not looked at.
export function reviewHasUnseen(
  review: { threadLatestMessage: Record<string, bigint> },
  seen: Record<string, bigint> | undefined,
): boolean {
  for (const [thread, latest] of Object.entries(review.threadLatestMessage)) {
    if ((seen?.[thread] ?? 0n) < latest) return true;
  }
  return false;
}

/// Spells the tab out, because a mark alone does not say what is in
/// there.
export function reviewTabTitle(
  label: string,
  review: {
    draftCount: number;
    openCount: number;
    answeredCount: number;
    resolvedCount: number;
  },
  unseen: boolean,
): string {
  const parts: string[] = [];
  if (unseen) parts.push("unseen replies");
  if (review.draftCount) parts.push(`${review.draftCount} unsent`);
  if (review.answeredCount) parts.push(`${review.answeredCount} your turn`);
  if (review.openCount) parts.push(`${review.openCount} with agent`);
  if (review.resolvedCount) parts.push(`${review.resolvedCount} resolved`);
  return parts.length ? `${label} — ${parts.join(", ")}` : label;
}

/// True when the open review a selection names is gone/// True when the open review a selection names is gone, so the pane
/// falls back rather than rendering a review that no longer exists.
export function reviewSelectionWasRemoved(
  selection: string,
  reviewIds: ReadonlySet<string>,
): boolean {
  const id = selectedReviewId(selection);
  return id !== null && !reviewIds.has(id);
}

// No control opens the diagnostics bar: set this key to "1" and reload.
const DEBUG_BAR_STORAGE_KEY = "pm.terminal.debug";
const DEBUG_BAR_REFRESH_MS = 1_000;

// Re-export so tests that import from this module keep working.
export { shellTabLabel } from "@puppet-master/client-core/terminal/label";

export function terminalSelectionWasRemoved(
  selected: string,
  previousTerminalIds: ReadonlySet<string>,
  terminalIds: ReadonlySet<string>,
): boolean {
  return selected !== "agent" && previousTerminalIds.has(selected) && !terminalIds.has(selected);
}

// Selecting a tab navigates, so a pane retained behind another page must not
// reset its selection or it pulls the reader off that page.
export function selectionNeedsReset(
  active: boolean,
  sessionChanged: boolean,
  selected: string,
  previousTerminalIds: ReadonlySet<string>,
  terminalIds: ReadonlySet<string>,
): boolean {
  return active && (sessionChanged || terminalSelectionWasRemoved(selected, previousTerminalIds, terminalIds));
}

export function removedTerminalIds(
  previousTerminalIds: ReadonlySet<string>,
  terminalIds: ReadonlySet<string>,
): string[] {
  return [...previousTerminalIds].filter((id) => !terminalIds.has(id));
}

/** The pane header's line: where things stand, else the current step, else the session's name. */
export function sessionPaneTitle(session: Session): string {
  const text = session.summary.trim() || session.headline.trim();
  return text ? unescapeHtml(text) : sessionDisplayName(session);
}

export function canResumeSession(ended: boolean, resumable: boolean, taskPrompt: string): boolean {
  return ended && (resumable || taskPrompt.length === 0);
}

export function SessionPane({
  routeTab,
  routeFocus,
  onReviewFinished,
  sessionId,
  stage,
  onSelect,
  active = true,
}: {
  sessionId: string;
  stage: TerminalStage;
  onSelect: (id: string) => void;
  active?: boolean;
  /// The tab named by the URL; absent means the agent.
  routeTab?: string;
  routeFocus?: boolean;
  onReviewFinished?: () => void;
}) {
  const client = useClient();
  const state = useAppState();
  const now = useNow();
  const hostRef = useRef<HTMLDivElement>(null);
  const [error, setError] = useState<string | null>(null);
  // The URL is the only source of where you are inside a session, so
  // Back actually goes back. Returning from elsewhere carries the tab
  // along instead of this second-guessing an address that omitted it.
  const selectedTerminal = routeTab ?? "agent";
  const setSelectedTerminal = useCallback(
    (tab: string) =>
      navigate(
        sessionRoutePath(
          sessionId,
          tab,
          focusModeAfterLeaving(selectedTerminal, Boolean(routeFocus)),
        ),
      ),
    [sessionId, routeFocus, selectedTerminal],
  );
  const [contextOpen, setContextOpen] = useState(false);
  const [killConfirmOpen, setKillConfirmOpen] = useState(false);
  const [liveTitles, setLiveTitles] = useState<ReadonlyMap<string, string>>(new Map());
  const [debugOpen] = useState(
    () => localStorage.getItem(DEBUG_BAR_STORAGE_KEY) === "1",
  );
  const [debugSnapshots, setDebugSnapshots] = useState<TerminalLayerDebug[]>([]);

  useEffect(() => {
    if (!debugOpen || !active) return;
    const update = () => setDebugSnapshots(stage.debugSnapshot());
    update();
    const timer = setInterval(update, DEBUG_BAR_REFRESH_MS);
    return () => clearInterval(timer);
  }, [debugOpen, active, stage, sessionId, selectedTerminal]);

  const copyDebug = () => {
    const payload = {
      capturedAt: new Date().toISOString(),
      sessionId,
      selectedTerminal,
      layers: stage.debugSnapshot(),
    };
    void navigator.clipboard?.writeText(JSON.stringify(payload, null, 1)).catch(() => {});
  };

  const session = state.sessions.get(sessionId);
  const ended = session ? sessionEnded(session) : false;
  const forwards = useMemo(
    () => forwardsNewestFirst(forwardsForSession({ sessions: state.sessions, forwards: state.forwards }, sessionId)),
    [state.sessions, state.forwards, sessionId],
  );
  const [forwardsExpandedFor, setForwardsExpandedFor] = useState<string | null>(null);
  const forwardsExpanded = forwardsExpandedFor === sessionId;
  const { shown: shownForwards, hidden: hiddenForwards } = visibleForwards(forwards, forwardsExpanded);
  const terminals = useMemo(
    () => [...state.terminals.values()]
      .filter((terminal) => terminal.sessionId.toString() === sessionId)
      .sort((a, b) => Number(a.id - b.id)),
    [state.terminals, sessionId],
  );
  const reviews = useMemo(
    () => [...state.reviews.values()]
      .filter((review) => review.sessionId.toString() === sessionId)
      .sort((a, b) => Number(a.id - b.id)),
    [state.reviews, sessionId],
  );
  const reviewIds = useMemo(
    () => new Set(reviews.map((review) => review.id.toString())),
    [reviews],
  );
  const plans = useMemo(
    () => [...state.plans.values()]
      .filter((plan) => plan.owningSessionId.toString() === sessionId)
      .sort((a, b) => Number(a.id - b.id)),
    [state.plans, sessionId],
  );
  const planIds = useMemo(() => new Set(plans.map((plan) => plan.id.toString())), [plans]);
  // A review that was finished elsewhere must not leave the pane on a
  // tab that no longer exists.
  const openedReview = reviewSelectionWasRemoved(selectedTerminal, reviewIds)
    ? null
    : selectedReviewId(selectedTerminal);
  const selectedPlan = selectedPlanId(selectedTerminal);
  const openedPlan = selectedPlan !== null && planIds.has(selectedPlan) ? selectedPlan : null;
  const selected = terminals.find((t) => t.id.toString() === selectedTerminal);
  useEffect(() => {
    const host = hostRef.current;
    if (!host) return;
    stage.mount(host);
    return () => stage.unmount(host);
  }, [stage]);

  // The agent terminal may trail the session by one event right after a
  // spawn; wait for it rather than opening a terminal the client does
  // not know yet.
  const agentTerminal = terminals.find((terminal) => terminal.kind === TerminalKind.AGENT);
  useEffect(() => {
    if (!active) return;
    if (ended) {
      // Ended metadata remains selectable during the history grace period,
      // but its agent terminal must never be kept or recreated.
      stage.disposeSession(BigInt(sessionId));
      return;
    }
    if (selected) stage.showTerminal(selected.id);
    else if (agentTerminal) stage.show(BigInt(sessionId));
  }, [active, stage, sessionId, selected?.id, agentTerminal?.id, ended]);

  const previousInventory = useRef<{ sessionId: string; terminalIds: Set<string> }>({
    sessionId,
    terminalIds: new Set(),
  });
  useEffect(() => {
    const terminalIds = new Set(terminals.map((terminal) => terminal.id.toString()));
    const previous = previousInventory.current;
    previousInventory.current = { sessionId, terminalIds };
    if (previous.sessionId === sessionId) {
      for (const terminalId of removedTerminalIds(previous.terminalIds, terminalIds)) {
        stage.disposeTerminal(BigInt(terminalId));
      }
    }
    if (selectionNeedsReset(active, previous.sessionId !== sessionId, selectedTerminal, previous.terminalIds, terminalIds)) {
      setSelectedTerminal("agent");
    }
  }, [active, stage, sessionId, selectedTerminal, terminals]);

  useEffect(() => stage.onTerminalCommand((terminalId, title) => {
    setLiveTitles((current) => {
      const next = new Map(current);
      if (title) next.set(terminalId.toString(), title);
      else next.delete(terminalId.toString());
      return next;
    });
  }), [stage]);

  const canResume = session
    ? canResumeSession(ended, session.resumable, session.taskPrompt)
    : false;

  const project = session ? state.projects.get(session.projectId.toString()) : undefined;
  const worker =
    session && session.workerId !== LOCAL_WORKER_ID
      ? state.workers.get(session.workerId.toString())
      : undefined;
  const workerUnavailable = session && !state.workers.has(session.workerId.toString());
  const unavailableReason = session
    ? workerUnavailableReason(state.workers, session.workerId)
    : null;

  const run = (action: Promise<unknown>) => {
    action.catch((err: unknown) => {
      setError(err instanceof Error ? err.message : String(err));
    });
  };

  const activateForward = (event: MouseEvent, forwardId: string, url: string) => {
    event.preventDefault();
    run(openForward(browserJsonFetch, browserWindowOpener, forwardId, url));
  };

  const resume = () =>
    run(
      client.resumeSession(session!.id).then((outcome) => {
        if (outcome.createdId !== undefined) onSelect(outcome.createdId.toString());
      }),
    );

  const changeToken = session
    ? `${session.state}:${session.headline}:${session.stateDetail}`
    : "";

  return (
    <section className="pane">
      <header className="pane-head">
        {session ? (
          <>
            <button
              type="button"
              className="btn session-kill-button"
              aria-label={`Kill ${sessionDisplayName(session)}`}
              disabled={ended || Boolean(unavailableReason)}
              title={unavailableReason ?? "Stop this agent session"}
              onClick={() => setKillConfirmOpen(true)}
            >
              <svg viewBox="0 0 16 16" aria-hidden="true">
                <path d="M8 2v6M4.2 4.5a5 5 0 1 0 7.6 0" />
              </svg>
            </button>
            <StateBadge state={session.state} />
            <span className="pane-title" title={sessionPaneTitle(session)}>
              {sessionPaneTitle(session)}
            </span>
            <span className="agent-tag">{agentLabel(session.agent)}</span>
            {project && <span className="pane-project">{project.name}</span>}
            {worker && <WorkerChip worker={worker} />}
            {workerUnavailable && <UnavailableWorkerChip workerId={session.workerId} />}
            <span className="session-elapsed">{sessionElapsed(session, now)}</span>
            <div className="topbar-spacer" />
            {canResume && (
              <button
                type="button"
                className="btn btn-primary"
                disabled={Boolean(unavailableReason)}
                title={unavailableReason ?? undefined}
                onClick={resume}
              >
                {session.resumable ? "resume" : "restart"}
              </button>
            )}
          </>
        ) : (
          <span className="pane-title">session {sessionId}</span>
        )}
      </header>
      {error && (
        <div className="flash-error" role="alert">
          <span>{error}</span>
          <button type="button" className="btn" onClick={() => setError(null)}>
            dismiss
          </button>
        </div>
      )}
      {unavailableReason && (
        <div className="worker-recovery-banner" role="status">
          <strong>{unavailableReason}</strong>
          <span>Recovery is automatic when it reconnects. Terminal input is paused.</span>
        </div>
      )}
      {session && forwards.length > 0 && (
        <div className="forwards-bar">
          {shownForwards.map(({ forward }) => (
            <span className="forward-entry" key={forward.id.toString()}>
              {forward.sourcePath && (
                <span className="forward-kind" title={`shared directory ${forward.sourcePath}`}>
                  dir
                </span>
              )}
              {forward.url ? (
                <a
                  className="forward-link"
                  href={forward.url}
                  target="_blank"
                  rel="noreferrer"
                  title={forward.sourcePath ? `${forward.sourcePath} at ${forward.url}` : forward.url}
                  onClick={(event) => activateForward(event, forward.id.toString(), forward.url)}
                  onAuxClick={(event) => {
                    if (event.button === MIDDLE_BUTTON) activateForward(event, forward.id.toString(), forward.url);
                  }}
                >
                  {forwardName(forward)}
                </a>
              ) : (
                <>
                  <span className="forward-label" title={forward.sourcePath || undefined}>
                    {forwardName(forward)}
                  </span>
                  <span className="muted-line">unbound</span>
                </>
              )}
              {forward.targetReachable === false && (
                <span className="forward-dead">
                  {forward.sourcePath ? "not being served" : "server not running"}
                </span>
              )}
              <button
                type="button"
                className="btn forward-close"
                title={forward.sourcePath ? "stop sharing this directory" : "close this forward"}
                onClick={() => run(client.closeForward(forward.id))}
              >
                ×
              </button>
            </span>
          ))}
          {(hiddenForwards > 0 || forwardsExpanded) && (
            <button
              type="button"
              className="btn forwards-more"
              aria-expanded={forwardsExpanded}
              onClick={() => setForwardsExpandedFor(forwardsExpanded ? null : sessionId)}
            >
              {forwardsExpanded ? "fewer" : `${hiddenForwards} more`}
            </button>
          )}
        </div>
      )}
      <div className="term-wrap">
        <div className="terminal-tabs">
          <button type="button" className={`btn ${selectedTerminal === "agent" ? "active" : ""}`} onClick={() => setSelectedTerminal("agent")}>agent</button>
          {terminals.filter((t) => t.kind === TerminalKind.SHELL).map((terminal) => (
            <button
              type="button"
              className={`btn terminal-tab ${selectedTerminal === terminal.id.toString() ? "active" : ""}`}
              key={terminal.id.toString()}
              title={liveTitles.get(terminal.id.toString()) ?? stage.terminalCommand(terminal.id) ?? terminal.title}
              onClick={() => setSelectedTerminal(terminal.id.toString())}
            >
              {shellTabLabel(terminal.id, terminal.title, liveTitles.get(terminal.id.toString()) ?? stage.terminalCommand(terminal.id))}
            </button>
          ))}
          {reviews.map((review) => {
            const unseen = reviewHasUnseen(
              review,
              state.reviewViewerStates.get(review.id.toString())?.seen,
            );
            return (
              <a
                key={review.id.toString()}
                className={`btn terminal-tab review-tab ${openedReview === review.id.toString() ? "active" : ""}`}
                title={reviewTabTitle(review.label, review, unseen)}
                href={sessionTabHref(location, sessionId, reviewTabId(review.id))}
                target="_blank"
                rel="noreferrer"
                onClick={(event) => {
                  if (!opensOwnWindow(event)) return;
                  if (window.open(event.currentTarget.href, "_blank")) event.preventDefault();
                }}
              >
                <span className="review-tab-icon" aria-hidden="true">
                  <svg viewBox="0 0 16 16">
                    <path d="M2.5 2.5h11v8h-6l-3 3v-3h-2z" />
                    <path d="M5.5 5.5h5M5.5 7.5h3" />
                  </svg>
                </span>
                <span className="review-tab-name">{reviewTabLabel(review.label)}</span>
                {unseen && <span className="review-tab-unseen" aria-label="unseen replies" />}
              </a>
            );
          })}
          {plans.map((plan) => (
            <a
              key={plan.id.toString()}
              className={`btn terminal-tab plan-tab ${openedPlan === plan.id.toString() ? "active" : ""}`}
              title={`${plan.name} — ${plan.state === PlanState.ACTIVE ? "active" : plan.state === PlanState.ACCEPTED ? "accepted" : "archived"}`}
              href={sessionTabHref(location, sessionId, planTabId(plan.id))}
              target="_blank"
              rel="noreferrer"
            >
              <span className="plan-tab-icon" aria-hidden="true"><svg viewBox="0 0 16 16"><path d="M3 2.5h10v11H3zM5.5 5h5M5.5 7.5h5M5.5 10h3" /></svg></span>
              <span>{plan.name}</span>
              {plan.activeDecisionId !== undefined && <span className="plan-tab-attention" aria-label="decision waiting" />}
            </a>
          ))}
          <button type="button" className="btn" disabled={Boolean(unavailableReason)} title={unavailableReason ?? undefined} onClick={() => run(client.createShell(BigInt(sessionId)).then((outcome) => { if (outcome.createdId) setSelectedTerminal(outcome.createdId.toString()); }))}>+ Shell</button>
          {selected?.kind === TerminalKind.SHELL && <button type="button" className="btn btn-danger" disabled={Boolean(unavailableReason)} title={unavailableReason ?? undefined} onClick={() => run(client.closeTerminal(selected.id))}>close</button>}
          <div className="terminal-tabs-spacer" />
          {session && (
            <button
              type="button"
              className={`btn terminal-context-toggle ${contextOpen ? "active" : ""}`}
              aria-pressed={contextOpen}
              onClick={() => setContextOpen((v) => !v)}
            >
              info
            </button>
          )}
        </div>
        <div className="term-body">
          {openedReview && (
            <div className="term-review">
              <ReviewPage id={Number(openedReview)} onFinished={onReviewFinished} />
            </div>
          )}
          {openedPlan && (
            <div className="term-review plan-tab-body">
              <PlanWorkspace
                id={openedPlan}
                changeToken={`${state.plans.get(openedPlan)?.revision}:${state.plans.get(openedPlan)?.updatedAtUnixMs}:${state.plans.get(openedPlan)?.activeDecisionId}`}
              />
            </div>
          )}
          <div className="term-main" aria-hidden={openedReview || openedPlan ? true : undefined} inert={openedReview || openedPlan ? true : undefined}>
            <div className="term-host" ref={hostRef} />
            {session && ended && selectedTerminal === "agent" && (
              <div className="term-overlay">
                <div className="term-overlay-card">
                  <StateBadge state={session.state} />
                  <p className="term-overlay-line">
                    session {stateStyle(session.state).label}
                    {session.exitCode !== undefined ? ` with exit code ${session.exitCode}` : ""}
                  </p>
                  {session.stateDetail && <p className="term-overlay-detail">{session.stateDetail}</p>}
                  {canResume ? (
                    <button type="button" className="btn btn-primary" onClick={resume}>
                      {session.resumable ? "resume conversation" : "restart session"}
                    </button>
                  ) : (
                    <p className="term-overlay-detail muted-line">
                      no saved transcript for this session, so its conversation cannot be resumed
                    </p>
                  )}
                </div>
              </div>
            )}
            {!session && state.hydrated && (
              <div className="term-overlay">
                <div className="term-overlay-card">
                  <p className="term-overlay-line">this session no longer exists</p>
                </div>
              </div>
            )}
            {debugOpen && (
              <div className="terminal-debug-bar" role="status" aria-label="terminal diagnostics">
                <button type="button" className="btn terminal-debug-copy" onClick={copyDebug}>
                  copy
                </button>
                <div className="terminal-debug-lines">
                  {debugSnapshots.length === 0 && <span className="terminal-debug-line">collecting…</span>}
                  {debugSnapshots.map((layer) => (
                    <div className={`terminal-debug-line ${layer.visible ? "is-visible-layer" : ""}`} key={layer.key}>
                      {debugChips(layer, now).join("  ·  ")}
                    </div>
                  ))}
                </div>
              </div>
            )}
          </div>
          {session && contextOpen && (
            <ContextPanel
              context={state.contexts.get(sessionId)}
              sessionId={sessionId}
              changeToken={changeToken}
              summary={session.summary}
              activity={session.activity}
            />
          )}
        </div>
      </div>
      {session && killConfirmOpen && !ended && (
        <SessionKillDialog
          sessionName={sessionDisplayName(session)}
          onClose={() => setKillConfirmOpen(false)}
          onConfirm={() => client.killSession(session.id).then(() => setKillConfirmOpen(false))}
        />
      )}
    </section>
  );
}
