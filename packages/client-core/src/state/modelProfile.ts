import {
  AgentKind,
  ModelDialect,
  type AgentDialects,
  type Bucket,
  type ModelProfile,
  type ModelProfileEndpoint,
  type Project,
} from "../gen/pm/v1/pm_pb";

export type ModelProfileSourceName = "explicit" | "project" | "bucket";

export interface ResolvedModelProfile {
  profileId: bigint;
  source: ModelProfileSourceName;
}

/**
 * The profile a spawn would use: the explicit choice, else the
 * project's, else the bucket's. Mirrors the daemon's cascade so the UI
 * previews what the spawn will actually do.
 */
export function resolveModelProfile(
  project: Project | undefined,
  bucket: Bucket | undefined,
  explicit?: bigint,
): ResolvedModelProfile | undefined {
  if (explicit !== undefined) return { profileId: explicit, source: "explicit" };
  if (project?.modelProfileId !== undefined) {
    return { profileId: project.modelProfileId, source: "project" };
  }
  if (bucket?.modelProfileId !== undefined) {
    return { profileId: bucket.modelProfileId, source: "bucket" };
  }
  return undefined;
}

/**
 * The entry an agent would run, chosen in the adapter's own dialect
 * preference order. Undefined means the profile cannot serve that
 * agent and the spawn would be rejected.
 */
export function selectEndpoint(
  profile: ModelProfile | undefined,
  agent: AgentKind,
  agentDialects: readonly AgentDialects[],
): ModelProfileEndpoint | undefined {
  if (!profile) return undefined;
  const spoken = agentDialects.find((entry) => entry.agent === agent)?.dialects ?? [];
  for (const dialect of spoken) {
    const match = profile.endpoints.find((endpoint) => endpoint.dialect === dialect);
    if (match) return match;
  }
  return undefined;
}

/**
 * Whether a model profile can apply to an agent at all. An agent whose
 * CLI speaks no endpoint dialect always runs on its own account, so a
 * profile inherited from the project or bucket passes it by rather than
 * rejecting the spawn.
 */
export function profileApplies(
  agent: AgentKind,
  agentDialects: readonly AgentDialects[],
): boolean {
  return (agentDialects.find((entry) => entry.agent === agent)?.dialects.length ?? 0) > 0;
}

/** The agents a profile covers, derived from the daemon's capability list. */
export function coveredAgents(
  profile: ModelProfile,
  agentDialects: readonly AgentDialects[],
): AgentKind[] {
  return agentDialects
    .filter((entry) => selectEndpoint(profile, entry.agent, agentDialects) !== undefined)
    .map((entry) => entry.agent);
}

/** The agents that would run a given dialect's entry. */
export function agentsForDialect(
  dialect: ModelDialect,
  agentDialects: readonly AgentDialects[],
): AgentDialects[] {
  return agentDialects.filter((entry) => entry.dialects.includes(dialect));
}

/**
 * Whether any agent that would run this dialect's entry can apply a
 * background model. False means the field is inert for every such
 * agent, so the UI says so instead of taking a value it will drop.
 */
export function backgroundModelApplies(
  dialect: ModelDialect,
  agentDialects: readonly AgentDialects[],
): boolean {
  const agents = agentsForDialect(dialect, agentDialects);
  return agents.length === 0 || agents.some((entry) => entry.supportsBackgroundModel);
}

const DIALECT_LABELS: ReadonlyMap<ModelDialect, string> = new Map([
  [ModelDialect.ANTHROPIC_MESSAGES, "anthropic-messages"],
  [ModelDialect.OPENAI_RESPONSES, "openai-responses"],
  [ModelDialect.GOOGLE_GENAI, "google-genai"],
]);

export const DIALECTS: readonly ModelDialect[] = [
  ModelDialect.ANTHROPIC_MESSAGES,
  ModelDialect.OPENAI_RESPONSES,
  ModelDialect.GOOGLE_GENAI,
];

export function dialectLabel(dialect: ModelDialect): string {
  return DIALECT_LABELS.get(dialect) ?? "unspecified";
}

export function modelProfileSourceLabel(source: ModelProfileSourceName): string {
  return source === "explicit" ? "this session" : `the ${source}`;
}
