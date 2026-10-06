import { authedFetch } from "./token";

export type Access = "read" | "write" | "unknown";
export type Policy = "allow" | "approve" | "deny";
export interface ToolRule {
  access: Access;
  policy?: Policy | null;
}
export interface OAuthConfig {
  token_auth_method: "none" | "client_secret_post" | "client_secret_basic";
  authorization_url: string;
  token_url: string;
  client_id: string;
  scopes: string;
  redirect_uri: string;
  registration_endpoint?: string | null;
  /** A registered client is replaced on the next sign-in rather than reused. */
  registered?: boolean;
}
export interface ConnectionConfig {
  name: string;
  project_id: number;
  kind: "mcp" | "openapi";
  endpoint: string;
  schema: unknown | null;
  read_policy: Policy;
  write_policy: Policy;
  unknown_policy: Policy;
  rules: Record<string, ToolRule>;
  oauth: OAuthConfig | null;
}
export interface PolicyDraft {
  read_policy: Policy;
  write_policy: Policy;
  unknown_policy: Policy;
  rules: Record<string, ToolRule>;
}
export type ProposedTool = Pick<ConnectionTool, "name" | "description" | "suggested_access">;
/** What an update proposal changes besides policy, as the daemon computed it. */
export interface UpdateChanges {
  fields: {
    field: "name" | "kind" | "endpoint" | "project_id" | "oauth" | "schema";
    from?: unknown;
    to?: unknown;
  }[];
  tools: { added: ProposedTool[]; removed: string[]; changed: string[] };
  /** The update deactivates the connection until it is tested and activated again. */
  requires_activation: boolean;
}
export interface PolicyProposal {
  id: string;
  revision: number;
  session_id: number;
  policy: PolicyDraft;
  explanation: string;
  /** Present when the proposal also updates the connection's setup. */
  changes?: UpdateChanges | null;
}
export interface ConnectionTool {
  name: string;
  description: string;
  input_schema: unknown;
  suggested_access: Access;
  operation?: { method: string; path: string; unsupported?: string | null };
}
export interface Connection {
  id: number;
  revision: number;
  config: ConnectionConfig;
  active: boolean;
  created_by_session: number | null;
  credential_set: boolean;
  /** Which kind of credential is stored; only a single connection's details carry it. */
  credential_mode?: CredentialMode | null;
  tested_revision: number | null;
  tested_at?: number | null;
  /** When the user removed it; only a call's history still leads here. */
  deleted_at?: number | null;
  tools: ConnectionTool[];
  tool_count?: number;
  policy_proposal: PolicyProposal | null;
}
export type CredentialMode = "none" | "bearer" | "basic" | "api_key" | "oauth";
export interface ConnectionCall {
  id: string;
  session_id: number;
  project_id?: number;
  connection_id: number;
  connection_revision?: number;
  tool: string;
  arguments: unknown;
  justification: string;
  status: string;
  created_at: number;
  expires_at: number;
  decided_by?: string | null;
  decided_at?: number | null;
  finished_at?: number | null;
  requires_approval?: boolean;
  result: unknown | null;
  has_result?: boolean;
  error: string | null;
}
export async function connectionRequest<T>(
  path: string,
  body?: unknown,
  method = "POST",
): Promise<T> {
  const response = await authedFetch(
    path,
    body === undefined
      ? undefined
      : {
          method,
          headers: { "Content-Type": "application/json" },
          body: JSON.stringify(body),
        },
  );
  const value = await response.json();
  if (!response.ok)
    throw new Error(
      value.error ?? `Connection request failed (${response.status})`,
    );
  return value as T;
}
export const listConnections = () =>
  connectionRequest<Connection[]>("/api/connections");
export const listConnectionCalls = (sessionId?: string) =>
  connectionRequest<ConnectionCall[]>(
    `/api/connection-calls${sessionId ? `?session_id=${encodeURIComponent(sessionId)}` : ""}`,
  );
export function blankConnection(projectId: number): ConnectionConfig {
  return {
    name: "",
    project_id: projectId,
    kind: "mcp",
    endpoint: "",
    schema: null,
    read_policy: "allow",
    write_policy: "approve",
    unknown_policy: "approve",
    rules: {},
    oauth: null,
  };
}
export function effectivePolicy(config: PolicyDraft, name: string): Policy {
  const rule = config.rules[name];
  if (rule?.policy) return rule.policy;
  return rule?.access === "read"
    ? config.read_policy
    : rule?.access === "write"
      ? config.write_policy
      : config.unknown_policy;
}
/** The connection with the tool list an update proposal would leave it with. */
export function withProposedTools(connection: Connection, changes?: UpdateChanges | null): Connection {
  if (!changes) return connection;
  const removed = new Set(changes.tools.removed);
  return {
    ...connection,
    tools: [
      ...connection.tools.filter((tool) => !removed.has(tool.name)),
      ...changes.tools.added.map((tool) => ({ ...tool, input_schema: null })),
    ],
  };
}
export function policyChanges(connection: Connection, proposal: PolicyDraft) {
  return connection.tools
    .map((tool) => ({
      name: tool.name,
      beforeAccess: connection.config.rules[tool.name]?.access ?? "unknown",
      afterAccess: proposal.rules[tool.name]?.access ?? "unknown",
      before: effectivePolicy(connection.config, tool.name),
      after: effectivePolicy(proposal, tool.name),
      beforeOverride: connection.config.rules[tool.name]?.policy ?? null,
      afterOverride: proposal.rules[tool.name]?.policy ?? null,
    }))
    .filter(
      (row) =>
        row.before !== row.after ||
        row.beforeAccess !== row.afterAccess ||
        row.beforeOverride !== row.afterOverride,
    );
}
