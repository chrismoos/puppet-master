import type { AppState } from "@puppet-master/client-core/state/reducer";
import type { TerminalStreamStatus } from "@puppet-master/client-core/ws/terminalSocket";

/** How much a terminal's state asks of the user: wait, expect recovery, or act. */
export type TerminalStatusLevel = "wait" | "recovering" | "blocked";
export type TerminalStatusActionKind = "retry" | "reload" | "view-worker";

export interface TerminalStatusAction {
  readonly kind: TerminalStatusActionKind;
  readonly label: string;
  readonly primary?: boolean;
}

export interface TerminalStatusView {
  readonly level: TerminalStatusLevel;
  readonly title: string;
  readonly detail?: string;
  readonly meta?: string;
  readonly actions: readonly TerminalStatusAction[];
}

export interface TerminalStatusWorker {
  readonly name: string;
  readonly pmVersion?: string;
}

export interface TerminalStatusContext {
  /** No screen has been shown yet, so a wait is a first connect. */
  readonly connecting: boolean;
  readonly worker: TerminalStatusWorker | null;
}

/** The worker running a session, found through its terminal when that is all a pane knows. */
export function terminalStatusWorker(
  state: AppState,
  target: { readonly sessionId: bigint } | { readonly terminalId: bigint },
): TerminalStatusWorker | null {
  const sessionId = "sessionId" in target
    ? target.sessionId
    : state.terminals.get(target.terminalId.toString())?.sessionId;
  if (sessionId === undefined) return null;
  const session = state.sessions.get(sessionId.toString());
  const worker = session ? state.workers.get(session.workerId.toString()) : undefined;
  if (!worker) return null;
  return {
    name: worker.name || worker.hostname || `#${worker.id}`,
    ...(worker.pmVersion ? { pmVersion: worker.pmVersion } : {}),
  };
}

function sentence(text: string): string {
  return text ? `${text[0]!.toUpperCase()}${text.slice(1)}` : text;
}

export function terminalStatusView(
  status: TerminalStreamStatus,
  context: TerminalStatusContext,
): TerminalStatusView | null {
  if (status.phase === "online") return null;
  const worker = context.worker;
  switch (status.problem) {
    case "worker":
      return {
        level: "recovering",
        title: worker ? `Worker ${worker.name} is offline` : "This terminal's worker is offline",
        ...(status.lastError ? { detail: sentence(status.lastError) } : {}),
        meta: "This terminal reconnects when the worker is back.",
        actions: [{ kind: "view-worker", label: "View worker" }],
      };
    case "connection":
      return {
        level: "recovering",
        title: "Connection to this terminal lost",
        ...(status.lastError ? { detail: status.lastError } : {}),
        meta: "Retrying automatically. The screen shown is from before the connection dropped.",
        actions: [{ kind: "retry", label: "Retry now", primary: true }],
      };
    case "incompatible":
      return {
        level: "blocked",
        title: worker ? `Update worker ${worker.name} to open this terminal` : "Update this terminal's worker to open it",
        detail: "Its terminal engine doesn't match the controller's, so this session's screen can't be shown here. The session itself is unaffected.",
        ...(worker?.pmVersion ? { meta: `worker ${worker.pmVersion}` } : {}),
        actions: [{ kind: "view-worker", label: "View worker" }, { kind: "retry", label: "Try again" }],
      };
    case "page-outdated":
      return {
        level: "blocked",
        title: "Reload to open this terminal",
        detail: "The controller was updated and its terminal engine changed. This page still has the old one.",
        actions: [{ kind: "reload", label: "Reload page", primary: true }],
      };
    default:
      return {
        level: "wait",
        title: context.connecting ? "Connecting…" : "Reconnecting…",
        actions: [],
      };
  }
}

/** A renderer that fails to start leaves nothing to show until the page loads again. */
export function terminalRendererFailureView(message: string): TerminalStatusView {
  return {
    level: "blocked",
    title: "This terminal couldn't be drawn",
    detail: message,
    actions: [{ kind: "reload", label: "Reload page", primary: true }],
  };
}

/** Fills the status element for a view, or hides it for none. */
export function renderTerminalStatus(
  el: HTMLElement,
  view: TerminalStatusView | null,
  onAction: (kind: TerminalStatusActionKind) => void,
): void {
  el.replaceChildren();
  el.hidden = view === null;
  el.className = "terminal-stream-status";
  if (!view) {
    el.removeAttribute("role");
    return;
  }
  el.classList.add(`is-${view.level}`);
  el.setAttribute("role", view.level === "blocked" ? "alert" : "status");
  const doc = el.ownerDocument;
  const box = doc.createElement("div");
  box.className = "terminal-status-box";
  if (view.level === "wait") {
    const spinner = doc.createElement("span");
    spinner.className = "terminal-status-spinner";
    spinner.setAttribute("aria-hidden", "true");
    box.append(spinner);
  }
  const body = doc.createElement("div");
  body.className = "terminal-status-body";
  const title = doc.createElement("div");
  title.className = "terminal-status-title";
  title.textContent = view.title;
  body.append(title);
  for (const [text, className] of [[view.detail, "terminal-status-detail"], [view.meta, "terminal-status-meta"]] as const) {
    if (!text) continue;
    const line = doc.createElement("div");
    line.className = className;
    line.textContent = text;
    body.append(line);
  }
  box.append(body);
  if (view.actions.length > 0) {
    const actions = doc.createElement("div");
    actions.className = "terminal-status-actions";
    for (const action of view.actions) {
      const button = doc.createElement("button");
      button.type = "button";
      button.textContent = action.label;
      if (action.primary) button.classList.add("is-primary");
      button.addEventListener("click", () => onAction(action.kind));
      actions.append(button);
    }
    box.append(actions);
  }
  el.append(box);
}
