import { useEffect, useState } from "react";
import { connectionRoutePath, type ConnectionView } from "@puppet-master/client-core/router";
import {
  connectionRequest,
  effectivePolicy,
  type Access,
  type Connection,
  type ConnectionCall,
  type ConnectionConfig,
  type Policy,
} from "../api/connections";
import { copyTextToClipboard } from "../clipboard";
import { navigate, replaceRoute } from "../router";
import { ConfirmDialog } from "./ConfirmDialog";
import { useAppState } from "../state/hooks";
import {
  callMatches,
  callStatus,
  callTimeline,
  KIND_LABEL,
  policyLine,
  proposalLine,
  shortEndpoint,
  toolAccess,
  toolCounts,
  toolMatches,
  toolSummary,
  untilLabel,
  type CallStatusFilter,
  type ToolFilter,
} from "./connectionsModel";
import { Badge, CallsTable, Chips, PolicyBadge, useDecision, useNames } from "./connectionsParts";
import { Pager } from "./Pager";
import { DEFAULT_PAGE_SIZE, paginate } from "./pagination";
import {
  ConnectedEditor,
  ConnectionEditor,
  useConnections,
  useFullConnection,
} from "./ConnectionsPanel";

const POLICIES: Policy[] = ["allow", "approve", "deny"];
const ACCESS: Access[] = ["unknown", "read", "write"];
const POLICY_OPTION: Record<Policy, string> = { allow: "Allow", approve: "Approve", deny: "Deny" };
const CREDENTIAL_LABEL: Record<string, string> = {
  bearer: "Bearer token",
  api_key: "API key header",
  basic: "Basic",
  oauth: "OAuth",
};
const COPIED_MS = 1500;

function CopyLink() {
  const [copied, setCopied] = useState(false);
  useEffect(() => {
    if (!copied) return;
    const timer = setTimeout(() => setCopied(false), COPIED_MS);
    return () => clearTimeout(timer);
  }, [copied]);
  return (
    <button
      type="button"
      className="btn"
      onClick={() =>
        void copyTextToClipboard(window.location.href).then(
          () => setCopied(true),
          () => undefined,
        )
      }
    >
      {copied ? "Copied" : "Copy link"}
    </button>
  );
}

function clock(at: number): string {
  return new Date(at).toLocaleTimeString([], { hour12: false });
}

function CallModal({
  callId,
  summary,
  connections,
  refresh,
}: {
  callId: string;
  /** The listed record, whose status says when the full one is stale. */
  summary?: ConnectionCall;
  connections: readonly Connection[];
  refresh: () => void;
}) {
  const { projectName, sessionName } = useNames();
  const [call, setCall] = useState<ConnectionCall>();
  const [loadError, setLoadError] = useState("");
  const { busy, error, decide } = useDecision(refresh);
  const close = () => navigate(connectionRoutePath());
  useEffect(() => {
    let stopped = false;
    void connectionRequest<ConnectionCall>(`/api/connection-calls/${callId}`)
      .then((value) => {
        if (!stopped) {
          setCall(value);
          setLoadError("");
        }
      })
      .catch((error) => {
        if (!stopped)
          setLoadError(error instanceof Error ? error.message : "Could not load call details");
      });
    return () => {
      stopped = true;
    };
  }, [callId, summary?.status]);
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") close();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, []);
  const connection = call
    ? connections.find((candidate) => candidate.id === call.connection_id)
    : undefined;
  const full = useFullConnection(connection).full;
  const now = Date.now();
  const status = call ? callStatus(summary?.status ?? call.status) : null;
  const pending = (summary?.status ?? call?.status) === "pending";
  const policy = call && full ? policyLine(full.config, call.tool) : null;
  const expires = call && pending ? untilLabel(call.expires_at, now) : null;
  return (
    <div
      className="modal-backdrop"
      onClick={(event) => {
        if (event.target === event.currentTarget) close();
      }}
    >
      <div
        className="modal cx cx-call-modal"
        role="dialog"
        aria-modal="true"
        aria-labelledby="connection-call-title"
      >
        <div className="cx-modal-head">
          <div>
            <div className="cx-kicker">Connection call</div>
            <h2 id="connection-call-title">
              {call?.tool ?? "Loading call…"}{" "}
              {status && <Badge tone={status.tone}>{status.label}</Badge>}
            </h2>
          </div>
          <span className="cx-spacer" />
          <CopyLink />
          <button type="button" className="btn btn-quiet" onClick={close}>
            Close
          </button>
        </div>
        {loadError && (
          <p className="cx-status" role="alert">
            {loadError}
          </p>
        )}
        {call && (
          <>
            <dl className="cx-kv">
              <dt>Connection</dt>
              <dd>
                <a href={`#${connectionRoutePath({ view: "detail", id: call.connection_id })}`}>
                  {connection?.config.name ?? `Connection ${call.connection_id}`}
                </a>{" "}
                <span className="cx-dim">
                  {connection ? `· ${shortEndpoint(connection.config.endpoint)} ` : ""}
                  {call.connection_revision != null ? `· revision ${call.connection_revision}` : ""}
                </span>
              </dd>
              <dt>Requested by</dt>
              <dd>
                <a href={`#/session/${call.session_id}`}>
                  Session #{call.session_id} · {sessionName(call.session_id)}
                </a>
                {call.project_id != null && (
                  <span className="cx-dim"> · {projectName(call.project_id)}</span>
                )}
              </dd>
              <dt>Justification</dt>
              <dd className={call.justification ? "" : "cx-dim"}>
                {call.justification || "None given"}
              </dd>
              {policy && (
                <>
                  <dt>Policy</dt>
                  <dd>
                    {policy.text}
                    {policy.override && <span className="cx-dim"> (tool override)</span>}
                  </dd>
                </>
              )}
              {expires ? (
                <>
                  <dt>Expires</dt>
                  <dd>{expires}</dd>
                </>
              ) : (
                call.decided_by && (
                  <>
                    <dt>Decided by</dt>
                    <dd>{call.decided_by}</dd>
                  </>
                )
              )}
            </dl>
            <div>
              <h3>Timeline</h3>
              <ul className="cx-timeline">
                {callTimeline({ ...call, status: summary?.status ?? call.status }, now).map(
                  (entry) => (
                    <li key={`${entry.at}:${entry.text}`}>
                      <time dateTime={new Date(entry.at).toISOString()}>{clock(entry.at)}</time>
                      <span className={entry.aside ? "cx-dim" : ""}>{entry.text}</span>
                    </li>
                  ),
                )}
              </ul>
            </div>
            <div>
              <h3>Exact arguments</h3>
              <pre>{JSON.stringify(call.arguments, null, 2)}</pre>
            </div>
            <div>
              <h3>Result</h3>
              {call.result != null ? (
                <pre>{JSON.stringify(call.result, null, 2)}</pre>
              ) : (
                <pre className="cx-dim">
                  {call.error ?? "Nothing yet. A result appears here once the call has run."}
                </pre>
              )}
            </div>
            {error && (
              <p className="cx-status" role="alert">
                {error}
              </p>
            )}
            {pending && (
              <div className="cx-modal-actions">
                <span className="cx-dim cx-small">
                  Approving runs these exact arguments once.
                </span>
                <span className="cx-spacer" />
                <button
                  type="button"
                  className="btn"
                  disabled={busy}
                  onClick={() => void decide(call.id, false)}
                >
                  Deny
                </button>
                <button
                  type="button"
                  className="btn btn-primary"
                  disabled={busy}
                  onClick={() => void decide(call.id, true)}
                >
                  Approve and execute
                </button>
              </div>
            )}
          </>
        )}
      </div>
    </div>
  );
}

function ConnectionsList({
  connections,
  calls,
  error,
  refresh,
}: {
  connections: readonly Connection[];
  calls: readonly ConnectionCall[];
  error: string;
  refresh: () => void;
}) {
  const state = useAppState();
  const { projectName, sessionName } = useNames();
  const [query, setQuery] = useState("");
  const [callQuery, setCallQuery] = useState("");
  const [page, setPage] = useState(0);
  const [size, setSize] = useState<number>(DEFAULT_PAGE_SIZE);
  const now = Date.now();
  const listed = connections.filter((connection) =>
    `${connection.config.name} ${connection.config.kind} ${KIND_LABEL[connection.config.kind]} ${projectName(connection.config.project_id)}`
      .toLowerCase()
      .includes(query.trim().toLowerCase()),
  );
  const matching = calls.filter((call) => callMatches(call, callQuery, "all", sessionName));
  const shown = paginate(matching, page, size);
  const waiting = calls.filter((call) => call.status === "pending").length;
  return (
    <>
      <section className="cx-section">
        <div className="cx-section-head">
          <h2>Connections</h2>
          <span className="cx-count">{connections.length}</span>
          <span className="cx-spacer" />
          <input
            aria-label="Filter connections"
            placeholder="Filter connections"
            value={query}
            onChange={(event) => setQuery(event.target.value)}
          />
          {state.projects.size > 0 ? (
            <a className="btn btn-primary" href={`#${connectionRoutePath({ view: "new" })}`}>
              Add connection
            </a>
          ) : (
            <button type="button" className="btn btn-primary" disabled>
              Add connection
            </button>
          )}
        </div>
        <p className="cx-lede">
          MCP servers and REST APIs, with shared project credentials and tool
          policies.
        </p>
        {error && (
          <p className="cx-status" role="alert">
            {error}
          </p>
        )}
        <div className="cx-table-wrap">
          <table className="cx-table">
            <thead>
              <tr>
                <th>Name</th>
                <th>Kind</th>
                <th>Project</th>
                <th className="cx-hide-narrow">Endpoint</th>
                <th className="cx-num">Tools</th>
                <th>Status</th>
              </tr>
            </thead>
            <tbody>
              {listed.length === 0 && (
                <tr>
                  <td className="cx-empty" colSpan={6}>
                    {connections.length ? "No connections match." : "No connections yet."}
                  </td>
                </tr>
              )}
              {listed.map((connection) => {
                const path = connectionRoutePath({ view: "detail", id: connection.id });
                return (
                  <tr key={connection.id} className="cx-row-link" onClick={() => navigate(path)}>
                    <td>
                      <a
                        className="cx-name"
                        href={`#${path}`}
                        onClick={(event) => event.stopPropagation()}
                      >
                        {connection.config.name || `Connection ${connection.id}`}
                      </a>
                    </td>
                    <td>{KIND_LABEL[connection.config.kind]}</td>
                    <td>{projectName(connection.config.project_id)}</td>
                    <td className="cx-hide-narrow cx-dim">
                      {shortEndpoint(connection.config.endpoint)}
                    </td>
                    <td className="cx-num">{connection.tool_count || "—"}</td>
                    <td>
                      <Badge tone={connection.active ? "ok" : "muted"}>
                        {connection.active ? "active" : "draft"}
                      </Badge>
                      {connection.policy_proposal && (
                        <>
                          {" "}
                          <Badge tone="pending">policy to review</Badge>
                        </>
                      )}
                    </td>
                  </tr>
                );
              })}
            </tbody>
          </table>
        </div>
      </section>

      <section className="cx-section">
        <div className="cx-section-head">
          <h2>Approvals and recent calls</h2>
          <span className="cx-count">
            {waiting} waiting · {calls.length} most recent
          </span>
          <span className="cx-spacer" />
          <input
            aria-label="Filter connection calls"
            placeholder="Filter tool, status, or justification"
            value={callQuery}
            onChange={(event) => {
              setCallQuery(event.target.value);
              setPage(0);
            }}
          />
        </div>
        <div className="cx-table-wrap">
          <CallsTable
            calls={shown.rows}
            connections={connections}
            now={now}
            refresh={refresh}
            linkSessions
          />
          {matching.length > DEFAULT_PAGE_SIZE && (
            <Pager
              page={shown}
              onPage={setPage}
              size={size}
              onSize={(next) => {
                setSize(next);
                setPage(0);
              }}
            />
          )}
        </div>
      </section>
    </>
  );
}

function DefaultsDialog({
  config,
  busy,
  onSave,
  onClose,
}: {
  config: ConnectionConfig;
  busy: boolean;
  onSave: (defaults: Pick<ConnectionConfig, "read_policy" | "write_policy" | "unknown_policy">) => void;
  onClose: () => void;
}) {
  const [defaults, setDefaults] = useState({
    read_policy: config.read_policy,
    write_policy: config.write_policy,
    unknown_policy: config.unknown_policy,
  });
  return (
    <div
      className="modal-backdrop"
      onClick={(event) => {
        if (event.target === event.currentTarget) onClose();
      }}
    >
      <div className="modal cx" role="dialog" aria-modal="true" aria-label="Policy defaults">
        <h2 style={{ fontSize: 16 }}>Policy defaults</h2>
        <p className="cx-lede" style={{ margin: 0 }}>
          What a tool gets from its classification when it has no override.
        </p>
        <div className="cx-fields cx-fields-three" style={{ marginBottom: 0 }}>
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
                value={defaults[key]}
                onChange={(event) =>
                  setDefaults({ ...defaults, [key]: event.target.value as Policy })
                }
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
        <div className="cx-modal-actions">
          <button type="button" className="btn btn-quiet" onClick={onClose}>
            Cancel
          </button>
          <button
            type="button"
            className="btn btn-primary"
            disabled={busy}
            onClick={() => onSave(defaults)}
          >
            Save defaults
          </button>
        </div>
      </div>
    </div>
  );
}

function ConnectionPage({
  summary,
  calls,
  refresh,
}: {
  summary: Connection;
  calls: readonly ConnectionCall[];
  refresh: () => void;
}) {
  const { projectName, sessionName } = useNames();
  const { full, error: loadError, setFull } = useFullConnection(summary);
  const [busy, setBusy] = useState(false);
  const [message, setMessage] = useState("");
  const [error, setError] = useState("");
  const [editingDefaults, setEditingDefaults] = useState(false);
  const [confirmingRemoval, setConfirmingRemoval] = useState(false);
  const [toolQuery, setToolQuery] = useState("");
  const [toolFilter, setToolFilter] = useState<ToolFilter>("all");
  const [toolPage, setToolPage] = useState(0);
  const [toolSize, setToolSize] = useState<number>(DEFAULT_PAGE_SIZE);
  const [callQuery, setCallQuery] = useState("");
  const [callFilter, setCallFilter] = useState<CallStatusFilter>("all");
  const [callPage, setCallPage] = useState(0);
  const [callSize, setCallSize] = useState<number>(DEFAULT_PAGE_SIZE);
  const now = Date.now();
  const run = async (task: () => Promise<string | void>) => {
    setBusy(true);
    setError("");
    setMessage("");
    try {
      const said = await task();
      if (said) setMessage(said);
      refresh();
    } catch (error) {
      setError(error instanceof Error ? error.message : "Connection update failed");
    } finally {
      setBusy(false);
    }
  };
  if (!full)
    return (
      <p className="cx-status" role={loadError ? "alert" : "status"}>
        {loadError || "Loading connection…"}
      </p>
    );
  const { config } = full;
  const update = (values: Partial<ConnectionConfig>) =>
    run(async () => {
      setFull(
        await connectionRequest<Connection>(
          `/api/connections/${full.id}`,
          { revision: full.revision, config: { ...config, ...values } },
          "PUT",
        ),
      );
    });
  const setRule = (tool: string, rule: ConnectionConfig["rules"][string]) =>
    update({ rules: { ...config.rules, [tool]: rule } });
  const tested = full.tested_revision === full.revision;
  const counts = toolCounts(full.tools, config);
  const tools = paginate(
    full.tools.filter((tool) => toolMatches(tool, config, toolQuery, toolFilter)),
    toolPage,
    toolSize,
  );
  const own = calls.filter((call) => call.connection_id === full.id);
  const waiting = own.filter((call) => call.status === "pending").length;
  const shownCalls = paginate(
    own.filter((call) => callMatches(call, callQuery, callFilter, sessionName)),
    callPage,
    callSize,
  );
  const proposal = full.policy_proposal;
  const name = config.name || `Connection ${full.id}`;
  return (
    <>
      <div className="cx-crumbs">
        <a href={`#${connectionRoutePath()}`}>Connections</a> / {name}
      </div>
      <div className="cx-detail-head">
        <h2>{name}</h2>
        <Badge tone={full.active ? "ok" : "muted"}>
          {full.deleted_at ? "removed" : full.active ? "active" : "draft"}
        </Badge>
        <Badge tone="info">{KIND_LABEL[config.kind]}</Badge>
        <span className="cx-spacer" />
        <CopyLink />
        {!full.deleted_at && (
          <>
        <button
          type="button"
          className="btn"
          disabled={busy}
          onClick={() =>
            void run(async () => {
              const result = await connectionRequest<Connection>(
                `/api/connections/${full.id}/test`,
                {},
              );
              setFull(result);
              return `Test passed. ${result.tools.length} tools discovered.`;
            })
          }
        >
          Test connection
        </button>
        <a className="btn" href={`#${connectionRoutePath({ view: "setup", id: full.id })}`}>
          Edit setup
        </a>
        {full.active ? (
          <button
            type="button"
            className="btn btn-danger"
            disabled={busy}
            onClick={() =>
              void run(async () => {
                setFull(
                  await connectionRequest<Connection>(`/api/connections/${full.id}/active`, {
                    revision: full.revision,
                    active: false,
                  }),
                );
                return "Connection disabled. Pending calls canceled.";
              })
            }
          >
            Disable
          </button>
        ) : (
          <button
            type="button"
            className="btn btn-loud"
            disabled={busy || !tested}
            onClick={() =>
              void run(async () => {
                setFull(
                  await connectionRequest<Connection>(`/api/connections/${full.id}/active`, {
                    revision: full.revision,
                    active: true,
                  }),
                );
                return "Connection activated. Agents can now use its tools.";
              })
            }
          >
            Activate
          </button>
        )}
        <button
          type="button"
          className="btn btn-quiet btn-quiet-danger"
          disabled={busy}
          onClick={() => setConfirmingRemoval(true)}
        >
          Remove
        </button>
          </>
        )}
      </div>

      {confirmingRemoval && (
        <ConfirmDialog
          title="remove connection"
          titleId="connection-remove-title"
          confirmLabel="remove connection"
          busyLabel="removing…"
          onClose={() => setConfirmingRemoval(false)}
          onConfirm={async () => {
            await connectionRequest<Connection>(
              `/api/connections/${full.id}`,
              { revision: full.revision },
              "DELETE",
            );
            setConfirmingRemoval(false);
            refresh();
            navigate(connectionRoutePath());
          }}
        >
          <p>Remove “{name}”?</p>
          <p className="muted-line">
            Agents lose its tools and any waiting calls are canceled. Its call history stays readable.
          </p>
        </ConfirmDialog>
      )}

      {full.deleted_at && (
        <p className="cx-status" role="status">
          This connection was removed. Its calls stay on record below.
        </p>
      )}

      {proposal && (
        <div className="cx-proposal">
          <span>
            <b>
              {proposal.changes ? "Update" : "Policy"} proposal from session #{proposal.session_id}.
            </b>{" "}
            {proposalLine(full, proposal)}
          </span>
          <span className="cx-spacer" />
          <button
            type="button"
            className="btn"
            disabled={busy}
            onClick={() =>
              void run(async () => {
                setFull(
                  await connectionRequest<Connection>(`/api/connections/${full.id}/policy`, {
                    revision: proposal.revision,
                    proposal_id: proposal.id,
                    accept: false,
                  }),
                );
                return `${proposal.changes ? "Update" : "Policy"} proposal dismissed.`;
              })
            }
          >
            Dismiss
          </button>
          <a
            className="btn btn-primary"
            href={`#${connectionRoutePath({ view: "setup", id: full.id })}`}
          >
            Review
          </a>
        </div>
      )}

      <dl className="cx-facts">
        <div>
          <dt>Endpoint</dt>
          <dd title={config.endpoint}>{config.endpoint}</dd>
        </div>
        <div>
          <dt>Project</dt>
          <dd>{projectName(config.project_id)}</dd>
        </div>
        <div>
          <dt>Credentials</dt>
          <dd>
            {full.credential_set
              ? `Stored${full.credential_mode && CREDENTIAL_LABEL[full.credential_mode] ? ` · ${CREDENTIAL_LABEL[full.credential_mode]}` : ""}`
              : "None needed"}
          </dd>
        </div>
        <div>
          <dt>Last tested</dt>
          <dd>
            {tested
              ? `Revision ${full.revision} · passed`
              : full.tested_revision != null
                ? "Changed since the last test"
                : "Not tested"}
          </dd>
        </div>
        <div>
          <dt>Created by</dt>
          <dd>
            {full.created_by_session != null ? (
              <a href={`#/session/${full.created_by_session}`}>
                Session #{full.created_by_session}
              </a>
            ) : (
              "A user"
            )}
          </dd>
        </div>
      </dl>
      {error && (
        <p className="cx-status" role="alert">
          {error}
        </p>
      )}
      {message && (
        <p className="cx-status" role="status">
          {message}
        </p>
      )}

      <section className="cx-section" style={{ marginTop: 24 }}>
        <div className="cx-section-head">
          <h2>Tools and policy</h2>
          <span className="cx-count">
            {full.tools.length} {full.tools.length === 1 ? "tool" : "tools"}
          </span>
          <span className="cx-spacer" />
          <span className="cx-dim cx-small">
            Defaults: reads {config.read_policy} · writes {config.write_policy} · unclassified{" "}
            {config.unknown_policy}
          </span>
          <button type="button" className="btn" onClick={() => setEditingDefaults(true)}>
            Edit defaults
          </button>
        </div>
        <div className="cx-section-head">
          <input
            aria-label="Filter tools"
            placeholder="Filter tools by name or description"
            value={toolQuery}
            onChange={(event) => {
              setToolQuery(event.target.value);
              setToolPage(0);
            }}
          />
          <Chips
            label="Filter by effective policy"
            value={toolFilter}
            onChange={(value) => {
              setToolFilter(value);
              setToolPage(0);
            }}
            options={[
              { value: "all", label: "All", count: counts.all },
              { value: "allow", label: "Allow", count: counts.allow },
              { value: "approve", label: "Approve", count: counts.approve },
              { value: "deny", label: "Deny", count: counts.deny },
              { value: "unknown", label: "Unclassified", count: counts.unknown },
            ]}
          />
        </div>
        <div className="cx-table-wrap">
          <table className="cx-table">
            <thead>
              <tr>
                <th>Tool</th>
                <th className="cx-hide-narrow">Description</th>
                <th>Classification</th>
                <th>Override</th>
                <th>Effective</th>
              </tr>
            </thead>
            <tbody>
              {tools.rows.length === 0 && (
                <tr>
                  <td className="cx-empty" colSpan={5}>
                    {full.tools.length
                      ? "No tools match."
                      : "No tools discovered yet. Test the connection to discover its tools."}
                  </td>
                </tr>
              )}
              {tools.rows.map((tool) => (
                <tr key={tool.name}>
                  <td className="cx-name">{tool.name}</td>
                  <td className="cx-hide-narrow cx-wrap" title={toolSummary(tool)}>
                    {toolSummary(tool)}
                  </td>
                  <td>
                    <select
                      className="cx-select-inline"
                      aria-label={`Classification for ${tool.name}`}
                      disabled={busy}
                      value={toolAccess(config, tool.name)}
                      onChange={(event) =>
                        void setRule(tool.name, {
                          ...config.rules[tool.name],
                          access: event.target.value as Access,
                        })
                      }
                    >
                      {ACCESS.map((access) => (
                        <option key={access}>{access}</option>
                      ))}
                    </select>
                  </td>
                  <td>
                    <select
                      className="cx-select-inline"
                      aria-label={`Override for ${tool.name}`}
                      disabled={busy}
                      value={config.rules[tool.name]?.policy ?? ""}
                      onChange={(event) =>
                        void setRule(tool.name, {
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
              ))}
            </tbody>
          </table>
          <Pager
            page={tools}
            onPage={setToolPage}
            size={toolSize}
            onSize={(next) => {
              setToolSize(next);
              setToolPage(0);
            }}
          />
        </div>
      </section>

      <section className="cx-section">
        <div className="cx-section-head">
          <h2>Calls on this connection</h2>
          <span className="cx-count">
            {waiting} waiting · {own.length} total
          </span>
          <span className="cx-spacer" />
          <a className="cx-small" href={`#${connectionRoutePath()}`}>
            All connections' calls
          </a>
        </div>
        <div className="cx-section-head">
          <input
            aria-label="Filter calls"
            placeholder="Filter by tool, session, or justification"
            value={callQuery}
            onChange={(event) => {
              setCallQuery(event.target.value);
              setCallPage(0);
            }}
          />
          <Chips
            label="Filter by status"
            value={callFilter}
            onChange={(value) => {
              setCallFilter(value);
              setCallPage(0);
            }}
            options={[
              { value: "all", label: "All" },
              { value: "pending", label: "Needs approval", count: waiting },
              { value: "succeeded", label: "Succeeded" },
              { value: "failed", label: "Failed" },
              { value: "denied", label: "Denied" },
            ]}
          />
        </div>
        <div className="cx-table-wrap">
          <CallsTable calls={shownCalls.rows} now={now} refresh={refresh} />
          <Pager
            page={shownCalls}
            onPage={setCallPage}
            size={callSize}
            onSize={(next) => {
              setCallSize(next);
              setCallPage(0);
            }}
          />
        </div>
      </section>

      {editingDefaults && (
        <DefaultsDialog
          config={config}
          busy={busy}
          onClose={() => setEditingDefaults(false)}
          onSave={(defaults) => {
            setEditingDefaults(false);
            void update(defaults);
          }}
        />
      )}
    </>
  );
}

function Missing({ what }: { what: string }) {
  return (
    <p className="cx-status" role="alert">
      {what} <a href={`#${connectionRoutePath()}`}>Back to connections</a>
    </p>
  );
}

export function SettingsConnections({ view }: { view?: ConnectionView }) {
  const state = useAppState();
  const { connections, calls, error, loaded, refresh } = useConnections();
  const firstProject = Number(state.projects.values().next().value?.id ?? 0);
  const selected =
    view && (view.view === "detail" || view.view === "setup")
      ? connections.find((connection) => connection.id === view.id)
      : undefined;
  let body;
  if (view?.view === "new") {
    body = (
      <ConnectionEditor
        key="new"
        projectId={firstProject}
        onSaved={(_completed, saved) => {
          void refresh();
          // The draft now has an address of its own, which a reload or a link keeps.
          if (saved) replaceRoute(connectionRoutePath({ view: "setup", id: saved.id }));
        }}
        onClose={() => navigate(connectionRoutePath())}
      />
    );
  } else if (view?.view === "setup") {
    body = selected ? (
      <ConnectedEditor
        key={selected.id}
        connection={selected}
        onSaved={(completed) => {
          void refresh();
          if (completed) navigate(connectionRoutePath({ view: "detail", id: selected.id }));
        }}
        onClose={() => navigate(connectionRoutePath({ view: "detail", id: selected.id }))}
      />
    ) : loaded ? (
      <Missing what="This connection no longer exists." />
    ) : (
      <p className="cx-status" role="status">
        Loading connection…
      </p>
    );
  } else if (view?.view === "detail") {
    body = selected ? (
      <ConnectionPage key={selected.id} summary={selected} calls={calls} refresh={() => void refresh()} />
    ) : loaded ? (
      <Missing what="This connection no longer exists." />
    ) : (
      <p className="cx-status" role="status">
        Loading connection…
      </p>
    );
  } else {
    body = (
      <>
        <ConnectionsList
          connections={connections}
          calls={calls}
          error={error}
          refresh={() => void refresh()}
        />
        {view?.view === "call" && (
          <CallModal
            callId={view.callId}
            summary={calls.find((call) => call.id === view.callId)}
            connections={connections}
            refresh={() => void refresh()}
          />
        )}
      </>
    );
  }
  return (
    <div className="cx settings-connections">
      {body}
    </div>
  );
}
