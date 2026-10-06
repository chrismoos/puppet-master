import {
  effectivePolicy,
  policyChanges,
  withProposedTools as withTools,
  type Access,
  type Connection,
  type ConnectionCall,
  type ConnectionConfig,
  type ConnectionTool,
  type Policy,
  type PolicyDraft,
  type PolicyProposal,
  type UpdateChanges,
} from "../api/connections";

/** Matches the daemon, which tells a waiting agent a call has stalled after this long. */
export const CALL_STALLED_AFTER_MS = 5 * 60 * 1000;

const SECOND = 1000;
const MINUTE = 60 * SECOND;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

export type Tone = "ok" | "pending" | "bad" | "muted" | "info";

/** "2m ago", in the coarsest unit that fits. */
export function agoLabel(at: number, now: number): string {
  const elapsed = Math.max(0, now - at);
  if (elapsed < MINUTE) return `${Math.floor(elapsed / SECOND)}s ago`;
  if (elapsed < HOUR) return `${Math.floor(elapsed / MINUTE)}m ago`;
  if (elapsed < DAY) return `${Math.floor(elapsed / HOUR)}h ago`;
  return `${Math.floor(elapsed / DAY)}d ago`;
}

/** "in 23h 58m", or null once the moment has passed. */
export function untilLabel(at: number, now: number): string | null {
  const left = at - now;
  if (left <= 0) return null;
  if (left < HOUR) return `in ${Math.max(1, Math.ceil(left / MINUTE))}m`;
  return `in ${Math.floor(left / HOUR)}h ${Math.floor((left % HOUR) / MINUTE)}m`;
}

const CALL_STATUS: Record<string, { label: string; tone: Tone }> = {
  pending: { label: "needs approval", tone: "pending" },
  authorized: { label: "running", tone: "info" },
  executing: { label: "running", tone: "info" },
  succeeded: { label: "succeeded", tone: "ok" },
  failed: { label: "failed", tone: "bad" },
  denied: { label: "denied", tone: "bad" },
  expired: { label: "expired", tone: "muted" },
  canceled: { label: "canceled", tone: "muted" },
  outcome_unknown: { label: "outcome unknown", tone: "bad" },
};

export function callStatus(status: string): { label: string; tone: Tone } {
  return CALL_STATUS[status] ?? { label: status, tone: "muted" };
}

export type CallStatusFilter = "all" | "pending" | "succeeded" | "failed" | "denied";

export function callMatches(
  call: ConnectionCall,
  query: string,
  status: CallStatusFilter,
  sessionName: (id: number) => string,
): boolean {
  if (status !== "all" && call.status !== status) return false;
  const text = `${call.tool} ${callStatus(call.status).label} ${call.status} ${call.justification} ${sessionName(call.session_id)}`;
  return text.toLowerCase().includes(query.trim().toLowerCase());
}

export function toolAccess(config: PolicyDraft, name: string): Access {
  return config.rules[name]?.access ?? "unknown";
}

/** What a tool says about itself: its description, or the REST operation behind it. */
export function toolSummary(tool: ConnectionTool): string {
  if (tool.description) return tool.description;
  return tool.operation ? `${tool.operation.method.toUpperCase()} ${tool.operation.path}` : "";
}

export type ToolFilter = "all" | Policy | Access;

export function toolMatches(
  tool: ConnectionTool,
  config: PolicyDraft,
  query: string,
  filter: ToolFilter,
): boolean {
  const access = toolAccess(config, tool.name);
  const matchesFilter =
    filter === "all" ||
    (filter === "allow" || filter === "approve" || filter === "deny"
      ? effectivePolicy(config, tool.name) === filter
      : access === filter);
  if (!matchesFilter) return false;
  return `${tool.name} ${toolSummary(tool)}`.toLowerCase().includes(query.trim().toLowerCase());
}

export function toolCounts(tools: readonly ConnectionTool[], config: PolicyDraft) {
  const counts = { all: tools.length, allow: 0, approve: 0, deny: 0, read: 0, write: 0, unknown: 0 };
  for (const tool of tools) {
    counts[effectivePolicy(config, tool.name)] += 1;
    counts[toolAccess(config, tool.name)] += 1;
  }
  return counts;
}

/** Rules with every unclassified tool given the classification its server or method suggests. */
export function withSuggestions(
  tools: readonly ConnectionTool[],
  rules: ConnectionConfig["rules"],
): ConnectionConfig["rules"] {
  const next = { ...rules };
  for (const tool of tools) {
    if ((rules[tool.name]?.access ?? "unknown") !== "unknown") continue;
    if (tool.suggested_access === "unknown") continue;
    next[tool.name] = { ...rules[tool.name], access: tool.suggested_access };
  }
  return next;
}

export const POLICY_PHRASE: Record<Policy, string> = {
  allow: "allow",
  approve: "approval required",
  deny: "deny",
};

/** "read · approval required (tool override)": how policy treats one tool. */
export function policyLine(config: PolicyDraft, tool: string): { text: string; override: boolean } {
  const access = toolAccess(config, tool);
  return {
    text: `${access === "unknown" ? "unclassified" : access} · ${POLICY_PHRASE[effectivePolicy(config, tool)]}`,
    override: Boolean(config.rules[tool]?.policy),
  };
}

export interface FieldChange {
  label: string;
  /** Absent for a field too large or structured to show as a pair of values. */
  from?: string;
  to?: string;
}

const KIND_NAME = { mcp: "MCP server", openapi: "REST API" } as const;

/** The setup fields an update proposal changes, as a review lists them. */
export function updateFieldChanges(
  changes: UpdateChanges,
  projectName: (id: number) => string,
): FieldChange[] {
  return changes.fields.map(({ field, from, to }) => {
    switch (field) {
      case "name":
        return { label: "Name", from: String(from), to: String(to) };
      case "endpoint":
        return { label: "Endpoint", from: String(from), to: String(to) };
      case "kind":
        return {
          label: "Type",
          from: KIND_NAME[from as keyof typeof KIND_NAME],
          to: KIND_NAME[to as keyof typeof KIND_NAME],
        };
      case "project_id":
        return { label: "Project", from: projectName(Number(from)), to: projectName(Number(to)) };
      case "oauth":
        return { label: "OAuth settings", from: from ? "Configured" : "None", to: to ? "Changed" : "None" };
      case "schema":
        return { label: "OpenAPI document" };
    }
  });
}

function counted(count: number, noun: string): string {
  return `${count} ${noun}${count === 1 ? "" : "s"}`;
}

/** One line saying what a proposal would change, setup first. */
export function proposalLine(connection: Connection, proposal: PolicyProposal): string {
  const policy = proposalSummary(withTools(connection, proposal.changes), proposal.policy);
  if (!proposal.changes) return policy;
  const { fields, tools } = proposal.changes;
  const parts = [];
  const named = fields.filter((change) => change.field !== "schema").length;
  if (named) parts.push(`${counted(named, "setting")} changed`);
  if (tools.added.length) parts.push(`${counted(tools.added.length, "tool")} added`);
  if (tools.removed.length) parts.push(`${counted(tools.removed.length, "tool")} removed`);
  if (tools.changed.length) parts.push(`${counted(tools.changed.length, "tool")} changed`);
  return parts.length ? `${parts.join(", ")}. Policy: ${policy}` : policy;
}

/** One line saying what a policy proposal would change. */
export function proposalSummary(connection: Connection, proposal: PolicyDraft): string {
  const changes = policyChanges(connection, proposal);
  const defaults = (["read_policy", "write_policy", "unknown_policy"] as const).filter(
    (key) => connection.config[key] !== proposal[key],
  ).length;
  if (changes.length === 1 && defaults === 0) {
    const [change] = changes;
    return change.before === change.after
      ? `${change.name}: ${change.beforeAccess} → ${change.afterAccess}.`
      : `${change.name}: ${POLICY_PHRASE[change.before]} → ${POLICY_PHRASE[change.after]}.`;
  }
  const parts = [];
  if (changes.length) parts.push(`${changes.length} tool ${changes.length === 1 ? "change" : "changes"}`);
  if (defaults) parts.push(`${defaults} default ${defaults === 1 ? "change" : "changes"}`);
  return parts.length ? `${parts.join(", ")}.` : "No changes to the current policy.";
}

export interface TimelineEntry {
  at: number;
  text: string;
  /** A note about the call rather than a step it took. */
  aside?: boolean;
}

/** What happened to a call, in order, as far as its record says. */
export function callTimeline(call: ConnectionCall, now: number): TimelineEntry[] {
  const held = call.requires_approval || call.status === "pending" || Boolean(call.decided_by);
  const entries: TimelineEntry[] = [
    { at: call.created_at, text: held ? "Requested, waiting for approval" : "Requested, allowed by policy" },
  ];
  const settledAt = call.decided_at ?? call.finished_at ?? (call.status === "pending" ? now : null);
  if (held && settledAt !== null && settledAt - call.created_at >= CALL_STALLED_AFTER_MS) {
    entries.push({
      at: call.created_at + CALL_STALLED_AFTER_MS,
      text: "Session told the call has stalled (5 min without a decision)",
      aside: true,
    });
  }
  if (call.decided_at != null) {
    entries.push({
      at: call.decided_at,
      text: `${call.status === "denied" ? "Denied" : "Approved"} by ${call.decided_by ?? "a user"}`,
    });
  }
  if (call.finished_at != null) {
    const label = callStatus(call.status).label;
    entries.push({
      at: call.finished_at,
      text: call.error ? `${capitalize(label)}: ${call.error}` : capitalize(label),
    });
  }
  return entries;
}

function capitalize(text: string): string {
  return text.charAt(0).toUpperCase() + text.slice(1);
}

/** An endpoint as a table shows it: host and path, without scheme, query or credentials. */
export function shortEndpoint(endpoint: string): string {
  try {
    const url = new URL(endpoint);
    return `${url.host}${url.pathname === "/" ? "" : url.pathname}`;
  } catch {
    return endpoint;
  }
}

export const KIND_LABEL: Record<ConnectionConfig["kind"], string> = { mcp: "MCP", openapi: "REST" };

export type SetupTab = "service" | "credentials" | "policy";
export const SETUP_TABS: readonly SetupTab[] = ["service", "credentials", "policy"];

export interface TabState {
  /** A check, a count that needs attention, or the tab's position. */
  mark: { kind: "done" } | { kind: "todo"; count: number } | { kind: "step"; step: number };
  summary: string;
}

/** The state each setup tab shows: whether it is finished and a one-line account of it. */
export function setupTabStates(
  saved: Connection | undefined,
  config: ConnectionConfig,
  projectName: string,
): Record<SetupTab, TabState> {
  const kind = config.kind === "mcp" ? "MCP server" : "REST API";
  const tested = Boolean(saved && saved.tested_revision === saved.revision);
  const tools = saved?.tools ?? [];
  const unclassified = tools.filter((tool) => toolAccess(config, tool.name) === "unknown").length;
  return {
    service: {
      mark: saved ? { kind: "done" } : { kind: "step", step: 1 },
      summary: `${kind} · ${projectName}`,
    },
    credentials: {
      mark: tested ? { kind: "done" } : { kind: "step", step: 2 },
      summary: tested
        ? `Passed · ${tools.length} ${tools.length === 1 ? "tool" : "tools"}`
        : saved?.tested_revision != null
          ? "Changed since the last test"
          : "Not tested",
    },
    policy: {
      mark: unclassified
        ? { kind: "todo", count: unclassified }
        : tools.length
          ? { kind: "done" }
          : { kind: "step", step: 3 },
      summary: !tools.length
        ? "No tools yet"
        : unclassified
          ? `${unclassified} unclassified`
          : "All classified",
    },
  };
}
