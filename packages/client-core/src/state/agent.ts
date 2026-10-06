import { AgentKind, type Bucket, type Project } from "../gen/pm/v1/pm_pb";

export type AgentSource = "explicit" | "project" | "bucket" | "fallback";

export interface ResolvedAgent {
  agent: AgentKind;
  source: AgentSource;
}

/** One agent's wire value and display name. */
export interface AgentOption {
  kind: AgentKind;
  value: string;
  label: string;
}

/**
 * The agents a spawn can choose, in picker order. Every agent control
 * renders this list rather than naming agents itself, so adding an
 * agent is one entry here. Kinds outside it exist on the wire but have
 * no adapter, so offering them would build a spawn the daemon rejects.
 */
export const AGENTS: readonly AgentOption[] = [
  { kind: AgentKind.CLAUDE_CODE, value: "claude", label: "Claude" },
  { kind: AgentKind.CODEX, value: "codex", label: "Codex" },
  { kind: AgentKind.GEMINI, value: "gemini", label: "Gemini" },
  { kind: AgentKind.OPENCODE, value: "opencode", label: "OpenCode" },
  { kind: AgentKind.ANTIGRAVITY, value: "antigravity", label: "Antigravity" },
];

const BY_KIND = new Map(AGENTS.map((agent) => [agent.kind, agent]));
const BY_VALUE = new Map(AGENTS.map((agent) => [agent.value, agent]));

export function resolveAgent(
  project: Project | undefined,
  bucket: Bucket | undefined,
  explicit?: AgentKind,
): ResolvedAgent {
  if (explicit !== undefined && explicit !== AgentKind.UNSPECIFIED) {
    return { agent: explicit, source: "explicit" };
  }
  if (project?.defaultAgent !== undefined && project.defaultAgent !== AgentKind.UNSPECIFIED) {
    return { agent: project.defaultAgent, source: "project" };
  }
  if (bucket?.defaultAgent !== undefined && bucket.defaultAgent !== AgentKind.UNSPECIFIED) {
    return { agent: bucket.defaultAgent, source: "bucket" };
  }
  return { agent: AgentKind.CLAUDE_CODE, source: "fallback" };
}

/** The select value for an agent; "" for none and for an agent with no adapter. */
export function agentValue(agent: AgentKind | undefined): string {
  return agent === undefined ? "" : (BY_KIND.get(agent)?.value ?? "");
}

export function agentFromValue(value: string): AgentKind | undefined {
  return BY_VALUE.get(value)?.kind;
}

export function agentLabel(agent: AgentKind): string {
  return BY_KIND.get(agent)?.label ?? AGENTS[0].label;
}
