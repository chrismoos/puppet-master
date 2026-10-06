import { AgentKind, SessionState, type Session } from "./gen/pm/v1/pm_pb";

/** The always-present local worker; sessions on it need no worker chip. */
export const LOCAL_WORKER_ID = 0n;

const MS_PER_SECOND = 1_000;
const SECONDS_PER_MINUTE = 60;
const MINUTES_PER_HOUR = 60;
const HOURS_PER_DAY = 24;

export function formatDuration(ms: number): string {
  const totalSeconds = Math.max(0, Math.floor(ms / MS_PER_SECOND));
  const seconds = totalSeconds % SECONDS_PER_MINUTE;
  const totalMinutes = Math.floor(totalSeconds / SECONDS_PER_MINUTE);
  const minutes = totalMinutes % MINUTES_PER_HOUR;
  const totalHours = Math.floor(totalMinutes / MINUTES_PER_HOUR);
  const hours = totalHours % HOURS_PER_DAY;
  const days = Math.floor(totalHours / HOURS_PER_DAY);
  if (days > 0) return `${days}d ${hours}h`;
  if (totalHours > 0) return `${hours}h ${String(minutes).padStart(2, "0")}m`;
  if (totalMinutes > 0) return `${minutes}m ${String(seconds).padStart(2, "0")}s`;
  return `${seconds}s`;
}

/** Live elapsed for running sessions, frozen total for ended ones. */
export function sessionElapsed(session: Session, now: number): string {
  const start = Number(session.createdAtUnixMs);
  const end = session.endedAtUnixMs !== undefined ? Number(session.endedAtUnixMs) : now;
  return formatDuration(end - start);
}

/** A single coarse unit like "10s", "5m", "3h", "2d". */
export function formatAgo(ms: number): string {
  const seconds = Math.max(0, Math.floor(ms / MS_PER_SECOND));
  if (seconds < SECONDS_PER_MINUTE) return `${seconds}s`;
  const minutes = Math.floor(seconds / SECONDS_PER_MINUTE);
  if (minutes < MINUTES_PER_HOUR) return `${minutes}m`;
  const hours = Math.floor(minutes / MINUTES_PER_HOUR);
  if (hours < HOURS_PER_DAY) return `${hours}h`;
  return `${Math.floor(hours / HOURS_PER_DAY)}d`;
}

/**
 * The activity clock the daemon derives for display: submitted user lines,
 * agent turn boundaries, and agent reports. PTY output and unsubmitted
 * typing never move it, so it is the one clock every surface reads.
 */
export function sessionLastActiveAt(session: Session): number {
  return Number(session.lastActivityAtUnixMs);
}

/** How long since the session last did something, e.g. "10s ago". */
export function sessionLastActive(session: Session, now: number): string {
  const last = sessionLastActiveAt(session);
  if (!last) return "";
  return `${formatAgo(now - last)} ago`;
}

const NOW_ACTIVITY_THRESHOLD_SECONDS = 10;

/** Compact age of the session's last activity for sidebar scanning. */
export function sessionLastActivity(session: Session, now: number): string {
  if (sessionEnded(session)) return "inactive";
  const last = sessionLastActiveAt(session);
  if (!last) return "unknown";
  const seconds = Math.max(0, Math.floor((now - last) / MS_PER_SECOND));
  if (seconds < NOW_ACTIVITY_THRESHOLD_SECONDS) return "now";
  return formatAgo(now - last);
}

export function sessionLastActivityTitle(session: Session, now: number): string {
  const activity = sessionLastActivity(session, now);
  if (activity === "inactive") return "Session inactive";
  if (activity === "unknown") return "Last activity unknown";
  if (activity === "now") return "Last activity: now";
  return `Last activity: ${activity} ago`;
}

export function agentLabel(agent: AgentKind): string {
  switch (agent) {
    case AgentKind.CLAUDE_CODE:
      return "claude";
    case AgentKind.CODEX:
      return "codex";
    case AgentKind.GEMINI:
      return "gemini";
    case AgentKind.OPENCODE:
      return "opencode";
    case AgentKind.ANTIGRAVITY:
      return "antigravity";
    case AgentKind.TEST:
      return "test";
    case AgentKind.UNSPECIFIED:
      return "unknown";
  }
}

const NAMED_ENTITIES: Record<string, string> = {
  amp: "&",
  lt: "<",
  gt: ">",
  quot: '"',
  apos: "'",
  nbsp: " ",
  mdash: "—",
  ndash: "–",
  hellip: "…",
  lsquo: "‘",
  rsquo: "’",
  ldquo: "“",
  rdquo: "”",
  copy: "©",
  reg: "®",
  trade: "™",
};

/** Decodes common HTML entities into plain text. */
export function unescapeHtml(text: string): string {
  if (!text || !text.includes("&")) return text;
  return text.replace(/&(#x[0-9a-fA-F]+|#[0-9]+|[a-zA-Z]+);/g, (match, entity: string) => {
    const lower = entity.toLowerCase();
    if (lower in NAMED_ENTITIES) {
      return NAMED_ENTITIES[lower];
    }
    if (lower.startsWith("#x")) {
      const code = parseInt(lower.slice(2), 16);
      if (!Number.isNaN(code) && code > 0) {
        try {
          return String.fromCodePoint(code);
        } catch {
          return match;
        }
      }
    } else if (lower.startsWith("#")) {
      const code = parseInt(lower.slice(1), 10);
      if (!Number.isNaN(code) && code > 0) {
        try {
          return String.fromCodePoint(code);
        } catch {
          return match;
        }
      }
    }
    return match;
  });
}

/** Names a session by what it is about: goal, then task title, then headline. */
export function sessionDisplayName(session: Session): string {
  const name = session.goal || session.taskTitle || session.headline;
  return name ? unescapeHtml(name) : `session ${session.id}`;
}

/**
 * The line under the session name: the current step, or the outcome once the
 * turn ends. Empty when the name already fell back to the headline.
 */
export function sessionStatusLine(session: Session): string {
  const headline = unescapeHtml(session.headline.trim());
  return headline && headline !== sessionDisplayName(session) ? headline : "";
}

/** Case-insensitive match of a list search query against the goal and headline. */
export function sessionMatchesSearch(session: Session, query: string): boolean {
  const needle = query.trim().toLowerCase();
  if (!needle) return true;
  return [sessionDisplayName(session), session.goal, session.headline]
    .some((text) => text.toLowerCase().includes(needle));
}

export interface StateStyle {
  label: string;
  className: string;
}

export function stateStyle(state: SessionState): StateStyle {
  switch (state) {
    case SessionState.STARTING:
      return { label: "starting", className: "st-starting" };
    case SessionState.WORKING:
      return { label: "working", className: "st-working" };
    case SessionState.NEEDS_INPUT:
      return { label: "needs input", className: "st-needs-input" };
    case SessionState.IDLE:
      return { label: "idle", className: "st-idle" };
    case SessionState.EXITED:
      return { label: "exited", className: "st-exited" };
    case SessionState.FAILED:
      return { label: "failed", className: "st-failed" };
    case SessionState.AWAITING_WORKER:
      return { label: "awaiting worker", className: "st-awaiting-worker" };
    case SessionState.UNSPECIFIED:
      return { label: "unknown", className: "st-exited" };
  }
}

export function sessionEnded(session: Session): boolean {
  return session.state === SessionState.EXITED || session.state === SessionState.FAILED;
}
