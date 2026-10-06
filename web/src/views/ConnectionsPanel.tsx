import {
  useCallback,
  useEffect,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
  type ReactNode,
} from "react";
import { connectionRoutePath } from "@puppet-master/client-core/router";
import {
  blankConnection,
  connectionRequest,
  effectivePolicy,
  listConnectionCalls,
  listConnections,
  policyChanges,
  withProposedTools,
  type Access,
  type Connection,
  type ConnectionCall,
  type ConnectionConfig,
  type CredentialMode,
  type Policy,
  type PolicyDraft,
} from "../api/connections";
import { startColumnResize } from "../columnResize";
import { useAppState } from "../state/hooks";
import {
  CONNECTION_PANEL_DISMISSED_KEY,
  CONNECTION_PANEL_MAX_WIDTH,
  CONNECTION_PANEL_MIN_WIDTH,
  CONNECTION_PANEL_WIDTH_KEY,
  readConnectionPanelWidth,
  readStringSet,
  writeString,
  writeStringSet,
} from "../storage";
import {
  agoLabel,
  callStatus,
  SETUP_TABS,
  setupTabStates,
  toolAccess,
  toolCounts,
  toolMatches,
  toolSummary,
  withSuggestions,
  type SetupTab,
  type TabState,
  type ToolFilter,
} from "./connectionsModel";
import { Badge, Chips, PolicyBadge, useDecision, useNames } from "./connectionsParts";
import { Pager } from "./Pager";
import { DEFAULT_PAGE_SIZE, paginate } from "./pagination";
import { ConnectionUpdateReview } from "./ConnectionUpdateReview";
import "./ConnectionsPanel.css";

const REFRESH_MS = 2000;
const MAX_OPENAPI_BYTES = 4 * 1024 * 1024;
const BYTES_PER_KB = 1024;
const POLICIES: Policy[] = ["allow", "approve", "deny"];
const ACCESS: Access[] = ["unknown", "read", "write"];
const HTTP_METHODS = ["get", "put", "post", "delete", "patch", "head", "options", "trace"];
const POLICY_LABELS: Record<Policy, string> = {
  allow: "Always allow",
  approve: "Require approval",
  deny: "Deny",
};
const POLICY_OPTION: Record<Policy, string> = { allow: "Allow", approve: "Approve", deny: "Deny" };
const ACCESS_LABELS: Record<Access, string> = {
  read: "Read",
  write: "Write",
  unknown: "Unclassified",
};
const TAB_LABEL: Record<SetupTab, string> = {
  service: "Service",
  credentials: "Credentials and test",
  policy: "Policy",
};
const TAB_LABEL_COMPACT: Record<SetupTab, string> = {
  service: "Service",
  credentials: "Credentials",
  policy: "Policy",
};
const AUTH_MODES: readonly { mode: CredentialMode; label: string }[] = [
  { mode: "none", label: "None" },
  { mode: "bearer", label: "Bearer token" },
  { mode: "api_key", label: "API key header" },
  { mode: "basic", label: "Basic" },
  { mode: "oauth", label: "OAuth" },
];

export function useConnections(enabled = true, sessionId?: string) {
  const [connections, setConnections] = useState<Connection[]>([]);
  const [calls, setCalls] = useState<ConnectionCall[]>([]);
  const [error, setError] = useState("");
  const [loaded, setLoaded] = useState(false);
  const refreshRef = useRef<() => Promise<void>>(async () => {});
  useEffect(() => {
    if (!enabled) return;
    let stopped = false;
    let busy = false;
    const refresh = async () => {
      if (busy) return;
      busy = true;
      try {
        const [connections, calls] = await Promise.all([
          listConnections(),
          listConnectionCalls(sessionId),
        ]);
        if (!stopped) {
          setConnections(connections);
          setCalls(calls);
          setError("");
          setLoaded(true);
        }
      } catch (error) {
        if (!stopped)
          setError(
            error instanceof Error
              ? error.message
              : "Could not load connections",
          );
      } finally {
        busy = false;
      }
    };
    refreshRef.current = refresh;
    void refresh();
    const timer = setInterval(() => void refresh(), REFRESH_MS);
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  }, [enabled, sessionId]);
  return { connections, calls, error, loaded, refresh: () => refreshRef.current() };
}

/** The full record of one connection, reloaded when the listed summary says it changed. */
export function useFullConnection(summary: Connection | undefined) {
  const [full, setFull] = useState<Connection>();
  const [error, setError] = useState("");
  const id = summary?.id;
  useEffect(() => {
    if (id === undefined) return;
    let stopped = false;
    void connectionRequest<Connection>(`/api/connections/${id}`)
      .then((value) => {
        if (!stopped) {
          setFull(value);
          setError("");
        }
      })
      .catch((error) => {
        if (!stopped)
          setError(
            error instanceof Error
              ? error.message
              : "Could not load connection",
          );
      });
    return () => {
      stopped = true;
    };
  }, [
    id,
    summary?.revision,
    summary?.active,
    summary?.tested_revision,
    summary?.policy_proposal?.id,
  ]);
  return { full: full?.id === id ? full : undefined, error, setFull };
}

export function PolicyReview({
  connection,
  policy,
}: {
  connection: Connection;
  policy: PolicyDraft;
}) {
  const [query, setQuery] = useState("");
  const [page, setPage] = useState(0);
  const changes = policyChanges(connection, policy);
  const matching = changes.filter((row) =>
    row.name.toLowerCase().includes(query.toLowerCase()),
  );
  const shown = paginate(matching, page, DEFAULT_PAGE_SIZE);
  return (
    <>
      <dl className="cx-review-defaults">
        {(
          [
            ["read_policy", "Reads"],
            ["write_policy", "Writes"],
            ["unknown_policy", "Unclassified tools"],
          ] as const
        ).map(([key, label]) => (
          <div key={key}>
            <dt>{label}</dt>
            <dd>
              {POLICY_LABELS[policy[key]]}
              {connection.config[key] !== policy[key] && (
                <small>
                  {" "}
                  Previously{" "}
                  {POLICY_LABELS[connection.config[key]].toLowerCase()}
                </small>
              )}
            </dd>
          </div>
        ))}
      </dl>
      <input
        aria-label="Filter proposed policy tools"
        placeholder="Filter proposed tool changes"
        value={query}
        onChange={(event) => {
          setQuery(event.target.value);
          setPage(0);
        }}
      />
      <div className="cx-table-wrap">
        <table className="cx-table">
          <thead>
            <tr>
              <th>Tool</th>
              <th>Current</th>
              <th>Proposed</th>
            </tr>
          </thead>
          <tbody>
            {shown.rows.map((row) => (
              <tr key={row.name}>
                <td className="cx-name" title={row.name}>
                  {row.name}
                </td>
                <td>
                  {ACCESS_LABELS[row.beforeAccess]}{" "}
                  <small className="cx-dim">
                    {POLICY_LABELS[row.before]} ·{" "}
                    {row.beforeOverride ? "Override" : "Default"}
                  </small>
                </td>
                <td>
                  {ACCESS_LABELS[row.afterAccess]}{" "}
                  <small className="cx-dim">
                    {POLICY_LABELS[row.after]} ·{" "}
                    {row.afterOverride ? "Override" : "Default"}
                  </small>
                </td>
              </tr>
            ))}
          </tbody>
        </table>
        <Pager page={shown} onPage={setPage} noun="tool changes" />
      </div>
    </>
  );
}

function operationCount(schemaText: string): number | null {
  try {
    const paths = (JSON.parse(schemaText) as { paths?: Record<string, object> }).paths ?? {};
    return Object.values(paths).reduce(
      (total, operations) =>
        total +
        Object.keys(operations ?? {}).filter((key) => HTTP_METHODS.includes(key.toLowerCase()))
          .length,
      0,
    );
  } catch {
    return null;
  }
}

function documentSummary(schemaText: string): string {
  if (!schemaText.trim()) return "No document yet";
  const operations = operationCount(schemaText);
  if (operations === null) return "Not valid JSON";
  const size = Math.max(1, Math.round(new Blob([schemaText]).size / BYTES_PER_KB));
  return `${operations} ${operations === 1 ? "operation" : "operations"} · ${size} KB`;
}

function TabMark({ mark }: { mark: TabState["mark"] }) {
  if (mark.kind === "done")
    return (
      <span className="cx-mark cx-mark-done" aria-label="done">
        ✓
      </span>
    );
  if (mark.kind === "todo")
    return (
      <span className="cx-mark cx-mark-todo" aria-label={`${mark.count} need attention`}>
        {mark.count}
      </span>
    );
  return (
    <span className="cx-mark" aria-hidden="true">
      {mark.step}
    </span>
  );
}

export function ConnectionEditor({
  connection,
  projectId,
  layout = "page",
  sessionId,
  onSaved,
  onClose,
}: {
  connection?: Connection;
  projectId: number;
  /** A page under Manage, or the narrow panel beside a session. */
  layout?: "page" | "panel";
  /** The session the panel sits beside. */
  sessionId?: string;
  onSaved: (completed: boolean, saved?: Connection) => void;
  onClose: () => void;
}) {
  const state = useAppState();
  const { projectName } = useNames();
  const panel = layout === "panel";
  const [saved, setSaved] = useState(connection);
  const [config, setConfig] = useState<ConnectionConfig>(
    connection?.config ?? blankConnection(projectId),
  );
  const [schemaUrl, setSchemaUrl] = useState("");
  const [schemaText, setSchemaText] = useState(
    connection?.config.schema
      ? JSON.stringify(connection.config.schema, null, 2)
      : "",
  );
  const [auth, setAuth] = useState<CredentialMode | "keep">("keep");
  const [credential, setCredential] = useState("");
  const [username, setUsername] = useState("");
  const [header, setHeader] = useState("X-API-Key");
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const [dirty, setDirty] = useState(false);
  const [savedAt, setSavedAt] = useState<number | null>(null);
  const tested = Boolean(saved && saved.tested_revision === saved.revision);
  const [tab, setTab] = useState<SetupTab>(
    connection?.policy_proposal || (connection && connection.tested_revision === connection.revision)
      ? "policy"
      : connection
        ? "credentials"
        : "service",
  );
  const tools = saved?.tools ?? [];
  const counts = toolCounts(tools, config);
  const [query, setQuery] = useState("");
  const [toolFilter, setToolFilter] = useState<ToolFilter | null>(null);
  const [toolPage, setToolPage] = useState(0);
  const [testTool, setTestTool] = useState("");
  const [testArguments, setTestArguments] = useState("{}");
  const [testResult, setTestResult] = useState<unknown>(null);
  // Unclassified tools are the ones that need a decision, so the table opens on them.
  const filter: ToolFilter = toolFilter ?? (counts.unknown ? "unknown" : "all");
  const shown = paginate(
    tools.filter((tool) => toolMatches(tool, config, query, filter)),
    toolPage,
    DEFAULT_PAGE_SIZE,
  );
  const changed = Boolean(
    connection && saved && connection.revision > saved.revision,
  );
  const now = Date.now();
  const tabs = setupTabStates(saved, config, projectName(config.project_id));
  const storedMode: CredentialMode = saved?.credential_mode ?? "none";
  const mode: CredentialMode = auth === "keep" ? (config.oauth ? "oauth" : storedMode) : auth;
  const keeping = auth === "keep" && Boolean(saved?.credential_set);
  const patch = (values: Partial<ConnectionConfig>) => {
    setConfig((previous) => ({ ...previous, ...values }));
    setDirty(true);
  };
  const patchRule = (tool: string, rule: ConnectionConfig["rules"][string]) =>
    patch({ rules: { ...config.rules, [tool]: rule } });
  const chooseAuth = (next: CredentialMode) => {
    setAuth(next === storedMode && saved?.credential_set ? "keep" : next);
    setCredential("");
    if (next !== "oauth" && config.oauth) patch({ oauth: null });
    setDirty(true);
  };
  const latest = useRef(saved);
  // A task returns true when it finishes setup, as opposed to saving a step.
  const run = async (task: () => Promise<boolean | void>) => {
    setBusy(true);
    setError("");
    setMessage("");
    try {
      const completed = (await task()) === true;
      await onSaved(completed, latest.current);
    } catch (error) {
      setError(
        error instanceof Error ? error.message : "Connection update failed",
      );
    } finally {
      setBusy(false);
    }
  };
  const keep = (next: Connection) => {
    latest.current = next;
    setSaved(next);
    setConfig(next.config);
  };
  const oauth = {
    token_auth_method: "none" as const,
    authorization_url: "",
    token_url: "",
    client_id: "",
    scopes: "",
    registration_endpoint: null,
    registered: false,
    ...config.oauth,
    redirect_uri:
      config.oauth?.redirect_uri ||
      `${window.location.origin}/api/connections/oauth/callback`,
  };
  const save = async () => {
    const next = {
      ...config,
      oauth: mode === "oauth" ? oauth : null,
      schema: config.kind === "openapi" ? JSON.parse(schemaText) : null,
    };
    let current = saved;
    if (!current)
      current = await connectionRequest<Connection>("/api/connections", next);
    const supplied =
      auth === "keep"
        ? undefined
        : auth === "none"
          ? { mode: "none" }
          : auth === "basic"
            ? { mode: "basic", username, password: credential }
            : auth === "api_key"
              ? { mode: "api_key", header, value: credential }
              : auth === "oauth"
                ? {
                    mode: "oauth",
                    access_token: "",
                    refresh_token: null,
                    expires_at: null,
                    client_secret: credential || null,
                  }
                : { mode: "bearer", token: credential };
    current = await connectionRequest<Connection>(
      `/api/connections/${current.id}`,
      { revision: current.revision, config: next, credential: supplied },
      "PUT",
    );
    keep(current);
    setDirty(false);
    setCredential("");
    setAuth("keep");
    setSavedAt(Date.now());
    setMessage(
      current.active
        ? "Connection changes saved."
        : "Draft saved. Test it before activation.",
    );
    return current;
  };
  const current = async () => (dirty || !saved ? save() : saved);
  const testConnection = async () => {
    const target = await current();
    const result = await connectionRequest<Connection>(
      `/api/connections/${target.id}/test`,
      {},
    );
    keep(result);
    setMessage(
      result.config.kind === "openapi"
        ? "Schema imported and API reachability checked. A HEAD request does not prove every operation accepts these credentials."
        : `Connected. Discovered ${result.tools.length} tools.`,
    );
  };
  const setActive = async (active: boolean) => {
    const target = await current();
    const updated = await connectionRequest<Connection>(
      `/api/connections/${target.id}/active`,
      { revision: target.revision, active },
    );
    keep(updated);
    setMessage(
      updated.active
        ? "Connection activated. Agents can now use its tools."
        : "Connection disabled. Pending calls canceled.",
    );
    return updated.active;
  };
  const proposal = connection?.policy_proposal;
  const adopt = (next: Connection) => {
    keep(next);
    setSchemaText(
      next.config.schema ? JSON.stringify(next.config.schema, null, 2) : "",
    );
    setAuth("keep");
    setCredential("");
    setDirty(false);
  };
  const reload = () => {
    if (!connection) return;
    adopt(connection);
    setMessage("Latest draft loaded.");
  };
  // With nothing typed here there is nothing to lose, so a change made by
  // the agent or another view is taken as it arrives.
  useEffect(() => {
    if (connection && changed && !dirty && !busy) adopt(connection);
  }, [connection, changed, dirty, busy]);
  // A proposal is new work to review, so it brings its tab forward.
  const proposalId = proposal?.id;
  useEffect(() => {
    if (proposalId) setTab("policy");
  }, [proposalId]);
  const name = config.name || (saved ? "Connection" : "New connection");
  const locked = busy || changed;

  const servicePane = (
    <>
      <div className="cx-kinds">
        {(
          [
            ["mcp", "MCP server", "An HTTP MCP endpoint. Tools are discovered from the server."],
            ["openapi", "REST API", "An OpenAPI 3 document. Each operation becomes a tool."],
          ] as const
        ).map(([kind, title, description]) => (
          <button
            key={kind}
            type="button"
            className="cx-kind"
            aria-pressed={config.kind === kind}
            disabled={locked}
            onClick={() => patch({ kind })}
          >
            <strong>{title}</strong>
            <span>{description}</span>
          </button>
        ))}
      </div>
      <div className="cx-fields">
        <label className="cx-field">
          <span>Name</span>
          <input
            value={config.name}
            onChange={(event) => patch({ name: event.target.value })}
          />
        </label>
        <label className="cx-field">
          <span>Project</span>
          <select
            aria-label="Project"
            value={config.project_id}
            onChange={(event) =>
              patch({ project_id: Number(event.target.value) })
            }
          >
            {[...state.projects.values()].map((project) => (
              <option key={project.id.toString()} value={Number(project.id)}>
                {project.name}
              </option>
            ))}
          </select>
        </label>
        <label className="cx-field cx-field-wide">
          <span>{config.kind === "mcp" ? "MCP endpoint" : "API base URL"}</span>
          <input
            type="url"
            value={config.endpoint}
            onChange={(event) => patch({ endpoint: event.target.value })}
          />
        </label>
        {config.kind === "openapi" && (
          <label className="cx-field cx-field-wide">
            <span>OpenAPI document</span>
            <input readOnly value={documentSummary(schemaText)} />
            <small>
              Replace from a file, a URL, or pasted JSON. Changing it requires a
              new test.
            </small>
          </label>
        )}
      </div>
      {config.kind === "openapi" && (
        <details className="cx-more" open={!schemaText.trim() || undefined}>
          <summary>Replace document</summary>
          <div className="cx-fields">
            <label className="cx-field">
              <span>OpenAPI document URL</span>
              <input
                type="url"
                value={schemaUrl}
                onChange={(event) => setSchemaUrl(event.target.value)}
              />
            </label>
            <label className="cx-field">
              <span>Import OpenAPI file</span>
              <input
                type="file"
                accept=".json,application/json"
                onChange={(event) => {
                  const file = event.target.files?.[0];
                  if (!file) return;
                  if (file.size > MAX_OPENAPI_BYTES) {
                    setError("OpenAPI file exceeds 4 MiB");
                    return;
                  }
                  void file.text().then((text) => {
                    setSchemaText(text);
                    setDirty(true);
                  });
                }}
              />
            </label>
            <div className="cx-field cx-field-wide">
              <button
                type="button"
                className="btn"
                style={{ alignSelf: "flex-start" }}
                disabled={!schemaUrl.trim()}
                onClick={() =>
                  void run(async () => {
                    const document = await connectionRequest<unknown>(
                      "/api/connections/import",
                      { url: schemaUrl },
                    );
                    setSchemaText(JSON.stringify(document, null, 2));
                    setDirty(true);
                    setMessage("Schema imported. Review it before saving.");
                  })
                }
              >
                Import OpenAPI URL
              </button>
            </div>
            <label className="cx-field cx-field-wide">
              <span>OpenAPI JSON</span>
              <textarea
                rows={6}
                value={schemaText}
                onChange={(event) => {
                  setSchemaText(event.target.value);
                  setDirty(true);
                }}
              />
            </label>
          </div>
        </details>
      )}
    </>
  );

  const secretLabel =
    mode === "oauth"
      ? "Client secret (optional)"
      : mode === "basic"
        ? "Password"
        : mode === "api_key"
          ? "API key"
          : "Token";
  const testButton = (
    <button
      type="button"
      className="btn"
      disabled={locked}
      onClick={() => void run(testConnection)}
    >
      {tested ? "Test again" : "Test connection"}
    </button>
  );
  const credentialsPane = (
    <>
      <div className="cx-fields">
        {panel ? (
          <label className="cx-field">
            <span>Authentication</span>
            <select
              aria-label="Authentication"
              value={mode}
              onChange={(event) => chooseAuth(event.target.value as CredentialMode)}
            >
              {AUTH_MODES.map((option) => (
                <option key={option.mode} value={option.mode}>
                  {option.label}
                </option>
              ))}
            </select>
          </label>
        ) : (
          <div className="cx-field cx-field-wide">
            <span>Authentication</span>
            <div className="cx-seg" role="group" aria-label="Authentication">
              {AUTH_MODES.map((option) => (
                <button
                  key={option.mode}
                  type="button"
                  aria-pressed={mode === option.mode}
                  onClick={() => chooseAuth(option.mode)}
                >
                  {option.label}
                </button>
              ))}
            </div>
          </div>
        )}
        {mode === "basic" && (
          <label className="cx-field">
            <span>Username</span>
            <input
              autoComplete="off"
              value={username}
              onChange={(event) => {
                setUsername(event.target.value);
                setAuth("basic");
                setDirty(true);
              }}
            />
          </label>
        )}
        {mode === "api_key" && (
          <label className="cx-field">
            <span>Header name</span>
            <input
              value={header}
              onChange={(event) => {
                setHeader(event.target.value);
                setAuth("api_key");
                setDirty(true);
              }}
            />
          </label>
        )}
        {mode !== "none" && (
          <label className={`cx-field ${mode === "bearer" || mode === "oauth" ? "cx-field-wide" : ""}`}>
            <span>{secretLabel}</span>
            <input
              type="password"
              autoComplete="new-password"
              aria-label={secretLabel}
              placeholder={keeping ? "••••••••••••••••••••" : ""}
              value={credential}
              onChange={(event) => {
                setCredential(event.target.value);
                setAuth(mode);
                setDirty(true);
              }}
            />
            {keeping && (
              <small>
                Stored. Leave as is to keep it, or type a new one to replace it.
              </small>
            )}
          </label>
        )}
      </div>
      {mode === "oauth" && (
        <details className="cx-more" open>
          <summary>OAuth endpoints</summary>
          <div className="cx-fields">
            {(
              [
                ["authorization_url", "Authorization URL"],
                ["token_url", "Token URL"],
                ["client_id", "Client ID"],
                ["scopes", "Scopes"],
                ["redirect_uri", "Redirect URI"],
              ] as const
            ).map(([key, label]) => (
              <label className="cx-field" key={key}>
                <span>{label}</span>
                <input
                  value={oauth[key]}
                  onChange={(event) =>
                    patch({ oauth: { ...oauth, [key]: event.target.value } })
                  }
                />
              </label>
            ))}
            <label className="cx-field">
              <span>Token endpoint authentication</span>
              <select
                aria-label="Token endpoint authentication"
                value={oauth.token_auth_method}
                onChange={(event) =>
                  patch({
                    oauth: {
                      ...oauth,
                      token_auth_method: event.target
                        .value as typeof oauth.token_auth_method,
                    },
                  })
                }
              >
                <option value="none">Public client (PKCE)</option>
                <option value="client_secret_basic">
                  Client secret in Basic header
                </option>
                <option value="client_secret_post">Client secret in form</option>
              </select>
            </label>
            <div className="cx-field cx-field-wide">
              <span className="cx-row-actions">
                {saved && (
                  <button
                    type="button"
                    className="btn"
                    onClick={() =>
                      void run(async () => {
                        const discovered = await connectionRequest<
                          ConnectionConfig["oauth"]
                        >(`/api/connections/${saved.id}/oauth/discover`, {
                          redirect_uri: oauth.redirect_uri,
                        });
                        patch({ oauth: discovered });
                        setMessage(
                          "OAuth endpoints discovered. Review them and enter your client ID.",
                        );
                      })
                    }
                  >
                    Discover OAuth endpoints
                  </button>
                )}
                {saved && config.oauth && (
                  <button
                    type="button"
                    className="btn"
                    disabled={dirty}
                    onClick={() => {
                      const popup = window.open("", "_blank");
                      if (popup) popup.opener = null;
                      void run(async () => {
                        try {
                          if (!oauth.client_id || oauth.registered) {
                            const registered = await connectionRequest<Connection>(
                              `/api/connections/${saved.id}/oauth/register`,
                              { redirect_uri: oauth.redirect_uri },
                            );
                            adopt(registered);
                          }
                          const { url } = await connectionRequest<{ url: string }>(
                            `/api/connections/${saved.id}/oauth/start`,
                            {},
                          );
                          if (popup) popup.location.assign(url);
                          else window.location.assign(url);
                        } catch (error) {
                          popup?.close();
                          throw error;
                        }
                        setMessage(
                          "Complete sign-in in the new tab, then reload this draft.",
                        );
                      });
                    }}
                  >
                    {!oauth.client_id || oauth.registered
                      ? "Register and sign in"
                      : "Sign in with OAuth"}
                  </button>
                )}
              </span>
              <small className="cx-dim">
                {oauth.registered
                  ? `Registered automatically as client ${oauth.client_id}. Signing in again registers a fresh client.`
                  : "Without a client ID, sign-in registers this installation with the server automatically. Providers that do not offer that need the client ID they issued."}
              </small>
            </div>
          </div>
        </details>
      )}
      <div
        className={`cx-test ${tested ? "cx-test-passed" : saved?.tested_revision != null ? "cx-test-stale" : ""}`}
        role="group"
        aria-label="Connection test"
      >
        {tested && saved ? (
          <>
            <strong>Test passed</strong>
            <ul>
              <li>Endpoint reachable</li>
              <li>
                {saved.credential_set ? "Credentials accepted" : "No credentials needed"}
              </li>
              <li>
                {tools.length} {tools.length === 1 ? "tool" : "tools"} discovered
              </li>
            </ul>
            {!panel && (
              <>
                <span className="cx-spacer" />
                <span className="cx-small">
                  revision {saved.revision}
                  {saved.tested_at ? ` · ${agoLabel(saved.tested_at, now)}` : ""}
                </span>
              </>
            )}
          </>
        ) : (
          <>
            <strong>
              {saved?.tested_revision != null ? "Changed since the last test" : "Not tested yet"}
            </strong>
            <span>
              Test the connection to check the credentials and discover its
              tools.
            </span>
          </>
        )}
      </div>
      {panel && <p style={{ margin: "12px 0 0" }}>{testButton}</p>}
      {saved && tested && config.kind === "openapi" && (
        <details className="cx-more" aria-label="Read operation test">
          <summary>Verify credentials with a read tool</summary>
          <div className="cx-fields">
            <label className="cx-field">
              <span>Read tool</span>
              <select
                aria-label="Verify credentials with a reviewed read tool"
                value={testTool}
                onChange={(event) => setTestTool(event.target.value)}
              >
                <option value="">Select a read tool</option>
                {tools
                  .filter((tool) => saved.config.rules[tool.name]?.access === "read")
                  .map((tool) => (
                    <option key={tool.name}>{tool.name}</option>
                  ))}
              </select>
            </label>
            <label className="cx-field">
              <span>Test arguments</span>
              <textarea
                value={testArguments}
                rows={3}
                onChange={(event) => setTestArguments(event.target.value)}
              />
            </label>
            <div className="cx-field cx-field-wide">
              <button
                type="button"
                className="btn"
                style={{ alignSelf: "flex-start" }}
                disabled={dirty || !testTool}
                onClick={() =>
                  void run(async () => {
                    const result = await connectionRequest<
                      Connection & { test_result: unknown }
                    >(`/api/connections/${saved.id}/test`, {
                      tool: testTool,
                      arguments: JSON.parse(testArguments),
                    });
                    keep(result);
                    setTestResult(result.test_result);
                    setMessage(
                      "Read tool test completed. Review its response below.",
                    );
                  })
                }
              >
                Test selected read tool
              </button>
              {testResult !== null && (
                <pre>{JSON.stringify(testResult, null, 2)}</pre>
              )}
            </div>
          </div>
        </details>
      )}
    </>
  );

  const suggestions = (
    <button
      type="button"
      className="btn"
      disabled={locked || counts.unknown === 0}
      onClick={() => patch({ rules: withSuggestions(tools, config.rules) })}
    >
      Use suggestions for all {counts.unknown}
    </button>
  );
  const classification = (tool: string) => (
    <select
      className="cx-select-inline"
      aria-label={`Classification for ${tool}`}
      value={toolAccess(config, tool)}
      onChange={(event) =>
        patchRule(tool, { ...config.rules[tool], access: event.target.value as Access })
      }
    >
      {ACCESS.map((access) => (
        <option key={access}>{access}</option>
      ))}
    </select>
  );
  const policyPane = (
    <>
      {proposal && connection && (
        <section className="cx-review" aria-label="Proposed connection policy">
          <h3>
            {proposal.changes ? "Agent update proposal" : "Agent policy proposal"}
            <span className="cx-spacer" />
            <Badge tone="pending">Your approval needed</Badge>
          </h3>
          <p>{proposal.explanation}</p>
          {proposal.changes && <ConnectionUpdateReview changes={proposal.changes} />}
          <PolicyReview
            connection={withProposedTools(connection, proposal.changes)}
            policy={proposal.policy}
          />
          <p className="cx-small cx-dim">
            Applying cancels pending calls authorized under the previous policy.
          </p>
          <div className="cx-review-actions">
            {[true, false].map((accept) => (
              <button
                key={String(accept)}
                type="button"
                className={accept ? "btn btn-primary" : "btn"}
                aria-label={
                  !accept
                    ? "Dismiss proposal"
                    : proposal.changes
                      ? "Apply proposed update"
                      : "Apply proposed policy"
                }
                disabled={busy || dirty || changed}
                onClick={() =>
                  void run(async () => {
                    const updated = await connectionRequest<Connection>(
                      `/api/connections/${connection.id}/policy`,
                      {
                        revision: proposal.revision,
                        proposal_id: proposal.id,
                        accept,
                      },
                    );
                    adopt(updated);
                    const subject = proposal.changes ? "Update" : "Policy";
                    setMessage(
                      !accept
                        ? `${subject} proposal dismissed.`
                        : updated.active || !proposal.changes
                          ? `${subject} applied.`
                          : "Update applied. Test and activate the connection to use it.",
                    );
                    return updated.active;
                  })
                }
              >
                {!accept ? "Dismiss" : proposal.changes ? "Apply update" : "Apply policy"}
              </button>
            ))}
            {!proposal.changes && <button
              type="button"
              className="btn"
              aria-label="Edit proposed policy"
              disabled={busy || changed}
              onClick={() => {
                patch(proposal.policy);
                setMessage(
                  "Proposal copied to the editor. Review your edits and save the policy to apply them.",
                );
              }}
            >
              Customize
            </button>}
          </div>
        </section>
      )}
      <div className="cx-fields cx-fields-three">
        {(
          [
            ["read_policy", "Reads"],
            ["write_policy", "Writes"],
            ["unknown_policy", "Unclassified"],
          ] as const
        ).map(([key, label]) => (
          <label className="cx-field" key={key}>
            <span>{label}</span>
            <select
              aria-label={label}
              value={config[key]}
              onChange={(event) => patch({ [key]: event.target.value as Policy })}
            >
              {POLICIES.map((policy) => (
                <option key={policy} value={policy}>
                  {POLICY_OPTION[policy]}
                </option>
              ))}
            </select>
          </label>
        ))}
      </div>
      {tools.length === 0 ? (
        <p role="note" className="cx-dim">
          No tools discovered yet. Test the connection to discover its tools,
          then classify them here.{" "}
          <button
            type="button"
            className="btn"
            disabled={locked}
            onClick={() => void run(testConnection)}
          >
            Test connection
          </button>
        </p>
      ) : (
        <>
          <div className="cx-section-head">
            <input
              aria-label="Filter connection tools"
              placeholder={panel ? "Filter tools" : "Filter tools by name or description"}
              value={query}
              onChange={(event) => {
                setQuery(event.target.value);
                setToolPage(0);
              }}
            />
            <Chips
              label="Filter by classification"
              value={filter}
              onChange={(value) => {
                setToolFilter(value);
                setToolPage(0);
              }}
              options={[
                { value: "all", label: "All", count: counts.all },
                { value: "unknown", label: "Unclassified", count: counts.unknown },
                { value: "read", label: "Read", count: counts.read },
                { value: "write", label: "Write", count: counts.write },
              ]}
            />
            {!panel && (
              <>
                <span className="cx-spacer" />
                {suggestions}
              </>
            )}
          </div>
          <div className="cx-table-wrap">
            <table className="cx-table">
              <thead>
                {panel ? (
                  <tr>
                    <th>Tool</th>
                    <th>Classification</th>
                    <th>Effective</th>
                  </tr>
                ) : (
                  <tr>
                    <th>Tool</th>
                    <th className="cx-hide-narrow">Description</th>
                    <th>Suggested</th>
                    <th>Classification</th>
                    <th>Override</th>
                    <th>Effective</th>
                  </tr>
                )}
              </thead>
              <tbody>
                {shown.rows.length === 0 && (
                  <tr>
                    <td className="cx-empty" colSpan={panel ? 3 : 6}>
                      No tools match.
                    </td>
                  </tr>
                )}
                {shown.rows.map((tool) =>
                  panel ? (
                    <tr key={tool.name} className="connection-tool">
                      <td>
                        <span className="cx-name">{tool.name}</span>
                        <br />
                        <span className="cx-dim cx-small">
                          suggested: {tool.suggested_access}
                        </span>
                      </td>
                      <td>{classification(tool.name)}</td>
                      <td>
                        <PolicyBadge policy={effectivePolicy(config, tool.name)} />
                      </td>
                    </tr>
                  ) : (
                    <tr key={tool.name} className="connection-tool">
                      <td className="cx-name">{tool.name}</td>
                      <td className="cx-hide-narrow cx-wrap" title={toolSummary(tool)}>
                        {tool.operation?.unsupported
                          ? `Unavailable: ${tool.operation.unsupported}`
                          : toolSummary(tool)}
                      </td>
                      <td className="cx-dim">{tool.suggested_access}</td>
                      <td>{classification(tool.name)}</td>
                      <td>
                        <select
                          className="cx-select-inline"
                          aria-label={`Override for ${tool.name}`}
                          value={config.rules[tool.name]?.policy ?? ""}
                          onChange={(event) =>
                            patchRule(tool.name, {
                              access: toolAccess(config, tool.name),
                              policy: (event.target.value as Policy) || null,
                            })
                          }
                        >
                          <option value="">use default</option>
                          {POLICIES.map((policy) => (
                            <option key={policy}>{policy}</option>
                          ))}
                        </select>
                      </td>
                      <td>
                        <PolicyBadge policy={effectivePolicy(config, tool.name)} />
                      </td>
                    </tr>
                  ),
                )}
              </tbody>
            </table>
            <Pager
              page={shown}
              onPage={setToolPage}
              compact={panel}
              noun={!panel && filter === "unknown" ? "unclassified" : undefined}
            />
          </div>
          {panel && <p style={{ margin: "10px 0 0" }}>{suggestions}</p>}
        </>
      )}
    </>
  );

  const panes: Record<SetupTab, { lede: string; body: ReactNode; aside?: ReactNode }> = {
    service: {
      lede: "What agents connect to, and which project may use it.",
      body: servicePane,
    },
    credentials: {
      lede: "Shared within the project and never shown to agents. One test checks them and discovers the tools.",
      body: credentialsPane,
      aside: testButton,
    },
    policy: {
      lede: panel
        ? "What agents may do without asking."
        : "What agents may do without asking. A tool with no classification uses the unclassified default.",
      body: policyPane,
      aside: counts.unknown ? (
        <Badge tone="pending">{counts.unknown} unclassified</Badge>
      ) : undefined,
    },
  };
  const tabList = (
    <div className="cx-tabs" role="tablist" aria-label="Setup sections">
      {SETUP_TABS.map((key) => (
        <button
          key={key}
          type="button"
          role="tab"
          aria-selected={tab === key}
          onClick={() => setTab(key)}
        >
          <TabMark mark={tabs[key].mark} />
          {panel ? TAB_LABEL_COMPACT[key] : TAB_LABEL[key]} <small>{tabs[key].summary}</small>
        </button>
      ))}
    </div>
  );
  const stale = changed && dirty && (
    <p className="cx-status cx-stale" role="alert">
      The agent or another view changed this draft.{" "}
      <button type="button" className="btn" onClick={reload}>
        Reload draft (discard local edits)
      </button>
    </p>
  );
  const notices = (
    <>
      {error && (
        <p className="cx-status form-error" role="alert">
          {error}
        </p>
      )}
      {message && (
        <p className="cx-status connection-feedback" role="status">
          {message}
        </p>
      )}
    </>
  );
  const saveButton = (
    <button
      type="button"
      className="btn"
      disabled={locked}
      onClick={() => void run(async () => (await save()).active)}
    >
      {saved?.active ? "Save changes" : "Save draft"}
    </button>
  );
  const activateButton = saved?.active ? (
    <button
      type="button"
      className="btn btn-danger"
      disabled={locked}
      onClick={() => void run(() => setActive(false))}
    >
      {panel ? "Disable" : "Disable connection"}
    </button>
  ) : (
    <button
      type="button"
      className="btn btn-loud"
      disabled={locked || !tested}
      onClick={() => void run(() => setActive(true))}
    >
      {panel ? "Activate" : "Activate connection"}
    </button>
  );
  const stateBadge = saved?.active ? (
    <Badge tone="ok">active</Badge>
  ) : (
    <Badge tone="muted">draft</Badge>
  );

  if (panel)
    return (
      <section className="cx cx-side" aria-label="Connection setup">
        <header className="cx-side-head">
          <div>
            <div className="cx-kicker">Connection setup</div>
            <h2>
              {name} {stateBadge}
            </h2>
          </div>
          <span className="cx-spacer" />
          {saved && (
            <a
              className="btn"
              title="Open the full setup page"
              href={`#${connectionRoutePath({ view: "setup", id: saved.id })}`}
            >
              Open page
            </a>
          )}
          <button type="button" className="btn connection-close" aria-label="Close" onClick={onClose}>
            ✕
          </button>
        </header>
        {saved?.created_by_session != null &&
          String(saved.created_by_session) === sessionId && (
            <div className="cx-agent-note">
              <span>Prepared by this session's agent.</span>
            </div>
          )}
        {tabList}
        <div className="cx-side-body" role="tabpanel" aria-label={TAB_LABEL[tab]}>
          {stale}
          <p className="cx-lede">{panes[tab].lede}</p>
          <fieldset disabled={locked} style={{ border: 0, margin: 0, padding: 0, minWidth: 0 }}>
            {panes[tab].body}
          </fieldset>
          {notices}
        </div>
        <footer className="cx-side-foot">
          {saveButton}
          <span className="cx-spacer" />
          {activateButton}
        </footer>
      </section>
    );

  return (
    <section className="cx cx-setup" aria-label="Connection setup">
      <div className="cx-crumbs">
        <a href={`#${connectionRoutePath()}`}>Connections</a> /{" "}
        {saved ? (
          <>
            <a href={`#${connectionRoutePath({ view: "detail", id: saved.id })}`}>{name}</a> / Setup
          </>
        ) : (
          "New connection"
        )}
      </div>
      <div className="cx-detail-head">
        <h2>{saved ? `Set up ${name}` : "Add connection"}</h2>
        {stateBadge}
        <span className="cx-spacer" />
        <span className="cx-dim cx-small">
          {[
            saved?.created_by_session != null
              ? `Seeded by session #${saved.created_by_session}`
              : "",
            dirty ? "unsaved changes" : savedAt ? `saved ${agoLabel(savedAt, now)}` : "",
          ]
            .filter(Boolean)
            .join(" · ")}
        </span>
      </div>
      <div className="cx-setup-bar">
        <span className="cx-dim cx-small">
          {tested
            ? "Tested at this revision."
            : "Test the connection before activating it."}
        </span>
        <span className="cx-spacer" />
        <button type="button" className="btn btn-quiet" onClick={onClose}>
          Close
        </button>
        {saveButton}
        {activateButton}
      </div>
      {stale}
      {tabList}
      <div className="cx-card" role="tabpanel" aria-label={TAB_LABEL[tab]}>
        <h3>
          {TAB_LABEL[tab]}
          <span className="cx-spacer" />
          {panes[tab].aside}
        </h3>
        <p className="cx-lede">{panes[tab].lede}</p>
        <fieldset disabled={locked} style={{ border: 0, margin: 0, padding: 0, minWidth: 0 }}>
          {panes[tab].body}
        </fieldset>
      </div>
      {notices}
    </section>
  );
}

function CallDetails({
  call,
  field,
  inline = false,
}: {
  call: ConnectionCall;
  field: "arguments" | "result";
  /** Shows the value in place, with nothing to open first. */
  inline?: boolean;
}) {
  const [open, setOpen] = useState(inline);
  const [full, setFull] = useState<ConnectionCall>();
  const [error, setError] = useState("");
  useEffect(() => {
    if (!open) return;
    let stopped = false;
    setFull(undefined);
    setError("");
    void connectionRequest<ConnectionCall>(`/api/connection-calls/${call.id}`)
      .then((value) => {
        if (!stopped) setFull(value);
      })
      .catch((error) => {
        if (!stopped)
          setError(
            error instanceof Error
              ? error.message
              : "Could not load call details",
          );
      });
    return () => {
      stopped = true;
    };
  }, [open, call.id, call.status]);
  const label = field === "arguments" ? "Exact arguments" : "Result";
  const value = error ? (
    <p role="alert">{error}</p>
  ) : full ? (
    <pre>{JSON.stringify(full[field], null, 2)}</pre>
  ) : (
    <p role="status">Loading call details…</p>
  );
  if (inline)
    return (
      <div className="cx-approval-args">
        <div className="cx-approval-label">{label}</div>
        {value}
      </div>
    );
  return (
    <details onToggle={(event) => setOpen(event.currentTarget.open)}>
      <summary>{label}</summary>
      {open && value}
    </details>
  );
}

const ACCESS_LABEL: Record<Access, string> = { read: "Read", write: "Write", unknown: "Unclassified" };
const ACCESS_TONE: Record<Access, "muted" | "pending"> = { read: "muted", write: "pending", unknown: "muted" };

function ApprovalCard({
  call,
  connection,
  refresh,
}: {
  call: ConnectionCall;
  connection?: Connection;
  refresh: () => void;
}) {
  const { busy, error, decide } = useDecision(refresh);
  const { sessionLabel } = useNames();
  const access = connection ? toolAccess(connection.config, call.tool) : undefined;
  const pending = call.status === "pending";
  return (
    <article className="cx cx-approval connection-approval">
      <header className="cx-approval-head">
        <p className="cx-approval-who">
          <b>{sessionLabel(call.session_id)}</b> {pending ? "asks to run" : "asked to run"}
        </p>
        <span
          className="cx-dim cx-small"
          title={new Date(call.created_at).toLocaleString()}
        >
          {agoLabel(call.created_at, Date.now())}
        </span>
      </header>
      <div className="cx-approval-tool">
        <h3>{call.tool}</h3>
        {access && <Badge tone={ACCESS_TONE[access]}>{ACCESS_LABEL[access]}</Badge>}
        {!pending && <Badge tone={callStatus(call.status).tone}>{callStatus(call.status).label}</Badge>}
      </div>
      <p className="cx-dim cx-small">
        {connection?.config.name ?? `Connection ${call.connection_id}`}
        {connection && ` · ${connection.config.endpoint}`}
      </p>
      {call.justification && <p className="cx-approval-why">{call.justification}</p>}
      <CallDetails call={call} field="arguments" inline={pending} />
      {pending && (
        <div className="cx-row-actions">
          <button
            type="button"
            className="btn btn-primary"
            disabled={busy}
            onClick={() => void decide(call.id, true)}
          >
            Approve and execute
          </button>
          <button
            type="button"
            className="btn btn-quiet"
            disabled={busy}
            onClick={() => void decide(call.id, false)}
          >
            Deny
          </button>
        </div>
      )}
      {(call.has_result || call.result != null) && (
        <CallDetails call={call} field="result" />
      )}
      {call.error && <p role="alert">{call.error}</p>}
      {error && <p role="alert">{error}</p>}
    </article>
  );
}

export function ConnectedEditor({
  connection,
  layout,
  sessionId,
  onSaved,
  onClose,
}: {
  connection: Connection;
  layout?: "page" | "panel";
  sessionId?: string;
  onSaved: (completed: boolean, saved?: Connection) => void;
  onClose: () => void;
}) {
  const { full, error } = useFullConnection(connection);
  if (!full)
    return (
      <section className="cx" aria-label="Connection setup" style={{ padding: 14 }}>
        <button type="button" className="btn btn-quiet" onClick={onClose}>
          Close
        </button>
        <p className="cx-status" role={error ? "alert" : "status"}>
          {error || "Loading connection setup…"}
        </p>
      </section>
    );
  return (
    <ConnectionEditor
      connection={full}
      projectId={full.config.project_id}
      layout={layout}
      sessionId={sessionId}
      onSaved={onSaved}
      onClose={onClose}
    />
  );
}

/** What the session panel shows, or null; dismissal is keyed to connection and proposal, so only new work reopens it. */
export function sessionPanelState({
  sessionId,
  connections,
  calls,
  selected,
  dismissed,
  dismissedCalls,
}: {
  sessionId: string;
  connections: readonly Connection[];
  calls: readonly ConnectionCall[];
  selected: number | null;
  dismissed: ReadonlySet<string>;
  dismissedCalls: ReadonlySet<string>;
}): { current: Connection | null; pending: ConnectionCall[] } | null {
  const relevant = connections.filter(
    (connection) =>
      String(connection.created_by_session) === sessionId ||
      String(connection.policy_proposal?.session_id) === sessionId,
  );
  const pending = calls.filter(
    (call) =>
      String(call.session_id) === sessionId &&
      call.status === "pending" &&
      !dismissedCalls.has(call.id),
  );
  const offered = relevant.find(
    (connection) =>
      (!connection.active || connection.policy_proposal) &&
      !dismissed.has(sessionPanelKey(sessionId, connection)),
  );
  const current =
    relevant.find((connection) => connection.id === selected) ?? offered ?? null;
  if (!current && pending.length === 0) return null;
  return { current, pending };
}

export function sessionPanelKey(sessionId: string, connection: Connection): string {
  return `${sessionId}:${connection.id}:${connection.policy_proposal?.id ?? "setup"}`;
}

export function ConnectionSessionPanel({
  sessionId,
}: {
  sessionId: string | null;
}) {
  const { connections, calls, refresh } = useConnections(
    Boolean(sessionId),
    sessionId ?? undefined,
  );
  const [selected, setSelected] = useState<number | null>(null);
  const [dismissed, setDismissed] = useState<Set<string>>(() =>
    readStringSet(CONNECTION_PANEL_DISMISSED_KEY),
  );
  const [dismissedCalls, setDismissedCalls] = useState<Set<string>>(new Set());
  const [width, setWidth] = useState(() => readConnectionPanelWidth());
  const [resizing, setResizing] = useState(false);
  const panelRef = useRef<HTMLElement>(null);
  const startResize = useCallback((event: ReactPointerEvent) => {
    const panel = panelRef.current;
    if (!panel) return;
    event.preventDefault();
    startColumnResize({
      originX: panel.getBoundingClientRect().right,
      anchor: "right",
      min: CONNECTION_PANEL_MIN_WIDTH,
      max: CONNECTION_PANEL_MAX_WIDTH,
      start: readConnectionPanelWidth(),
      onWidth: setWidth,
      onCommit: (committed) => writeString(CONNECTION_PANEL_WIDTH_KEY, String(committed)),
      onDragging: setResizing,
    });
  }, []);
  const view = sessionId
    ? sessionPanelState({ sessionId, connections, calls, selected, dismissed, dismissedCalls })
    : null;
  if (!sessionId || !view) return null;
  const { current, pending } = view;
  const close = (connection: Connection) => {
    const next = new Set(dismissed).add(sessionPanelKey(sessionId, connection));
    writeStringSet(CONNECTION_PANEL_DISMISSED_KEY, next);
    setDismissed(next);
    setSelected(null);
  };
  return (
    <aside
      ref={panelRef}
      className="connection-session-panel is-open"
      aria-label="Session connections"
      style={{ "--connection-panel-w": `${width}px` } as CSSProperties}
    >
      <div
        className={`cx-panel-resizer ${resizing ? "is-dragging" : ""}`}
        role="separator"
        aria-orientation="vertical"
        aria-label="resize connection panel"
        onPointerDown={startResize}
      />
      {current ? (
        <ConnectedEditor
          key={current.id}
          connection={current}
          layout="panel"
          sessionId={sessionId}
          onSaved={(completed) => {
            if (completed) close(current);
            else setSelected(current.id);
            void refresh();
          }}
          onClose={() => close(current)}
        />
      ) : (
        <header className="cx cx-side-head connection-panel-head">
          <h2>Approvals requested</h2>
          <span className="cx-spacer" />
          <button
            type="button"
            className="btn connection-close"
            onClick={() =>
              setDismissedCalls((previous) => {
                const next = new Set(previous);
                for (const call of pending) next.add(call.id);
                return next;
              })
            }
          >
            Close
          </button>
        </header>
      )}
      {pending.map((call) => (
        <ApprovalCard
          key={call.id}
          call={call}
          connection={connections.find(
            (connection) => connection.id === call.connection_id,
          )}
          refresh={() => void refresh()}
        />
      ))}
    </aside>
  );
}
