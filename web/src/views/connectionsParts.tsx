import { useState, type ReactNode } from "react";
import { sessionDisplayName } from "@puppet-master/client-core/format";
import { connectionRoutePath } from "@puppet-master/client-core/router";
import {
  connectionRequest,
  type Connection,
  type ConnectionCall,
  type Policy,
} from "../api/connections";
import { navigate } from "../router";
import { useAppState } from "../state/hooks";
import {
  agoLabel,
  callStatus,
  type Tone,
} from "./connectionsModel";

export function Badge({ tone, children }: { tone: Tone; children: ReactNode }) {
  return <span className={`badge cx-badge-${tone}`}>{children}</span>;
}

const POLICY_TONE: Record<Policy, Tone> = { allow: "ok", approve: "pending", deny: "bad" };

export function PolicyBadge({ policy }: { policy: Policy }) {
  return <Badge tone={POLICY_TONE[policy]}>{policy}</Badge>;
}

export function Chips<T extends string>({
  label,
  options,
  value,
  onChange,
}: {
  label: string;
  options: readonly { value: T; label: string; count?: number }[];
  value: T;
  onChange: (value: T) => void;
}) {
  return (
    <div className="cx-chips" role="group" aria-label={label}>
      {options.map((option) => (
        <button
          key={option.value}
          type="button"
          className="cx-chip"
          aria-pressed={option.value === value}
          onClick={() => onChange(option.value)}
        >
          {option.label}
          {option.count !== undefined && <b>{option.count}</b>}
        </button>
      ))}
    </div>
  );
}

/** Names for the ids a connection or call carries, from what the dashboard already holds. */
export function useNames() {
  const state = useAppState();
  return {
    projectName: (id: number) => state.projects?.get(String(id))?.name ?? `Project ${id}`,
    sessionLabel: (id: number) => {
      const session = state.sessions?.get(String(id));
      return session ? `#${id} ${sessionDisplayName(session)}` : `#${id}`;
    },
    sessionName: (id: number) => {
      const session = state.sessions?.get(String(id));
      return session ? sessionDisplayName(session) : `session ${id}`;
    },
  };
}

export function useDecision(refresh: () => void) {
  const [busy, setBusy] = useState(false);
  const [error, setError] = useState("");
  const decide = async (callId: string, approve: boolean) => {
    setBusy(true);
    setError("");
    try {
      await connectionRequest(`/api/connection-calls/${callId}/decision`, { approve });
      refresh();
    } catch (error) {
      setError(error instanceof Error ? error.message : "Decision failed");
    } finally {
      setBusy(false);
    }
  };
  return { busy, error, decide };
}

export function CallsTable({
  calls,
  connections,
  now,
  refresh,
  linkSessions,
}: {
  calls: readonly ConnectionCall[];
  /** Given on the list, which names each call's connection; a connection's own page leaves it out. */
  connections?: readonly Connection[];
  now: number;
  refresh: () => void;
  linkSessions?: boolean;
}) {
  const { sessionLabel } = useNames();
  const { busy, error, decide } = useDecision(refresh);
  const open = (call: ConnectionCall) =>
    navigate(connectionRoutePath({ view: "call", callId: call.id }));
  const columns = connections ? 7 : 6;
  return (
    <>
      <table className="cx-table">
        <thead>
          <tr>
            <th>When</th>
            <th>Status</th>
            <th>Tool</th>
            {connections && <th>Connection</th>}
            <th className="cx-hide-narrow">Session</th>
            <th className="cx-hide-narrow">Justification</th>
            <th>
              <span className="visually-hidden">Actions</span>
            </th>
          </tr>
        </thead>
        <tbody>
          {calls.length === 0 && (
            <tr>
              <td className="cx-empty" colSpan={columns}>
                No calls match.
              </td>
            </tr>
          )}
          {calls.map((call) => {
            const status = callStatus(call.status);
            return (
              <tr
                key={call.id}
                className="cx-row-link"
                tabIndex={0}
                aria-label={`${call.tool}, ${status.label}`}
                onClick={() => open(call)}
                onKeyDown={(event) => {
                  if (event.key === "Enter" && event.target === event.currentTarget) open(call);
                }}
              >
                <td>{agoLabel(call.created_at, now)}</td>
                <td>
                  <Badge tone={status.tone}>{status.label}</Badge>
                </td>
                <td className="cx-name">{call.tool}</td>
                {connections && (
                  <td>
                    {connections.find((connection) => connection.id === call.connection_id)
                      ?.config.name ?? `Connection ${call.connection_id}`}
                  </td>
                )}
                <td className="cx-hide-narrow">
                  {linkSessions ? (
                    <a
                      href={`#/session/${call.session_id}`}
                      onClick={(event) => event.stopPropagation()}
                    >
                      {sessionLabel(call.session_id)}
                    </a>
                  ) : (
                    sessionLabel(call.session_id)
                  )}
                </td>
                <td
                  className={`cx-hide-narrow cx-wrap ${call.justification ? "" : "cx-dim"}`}
                  title={call.justification}
                >
                  {call.justification || "—"}
                </td>
                <td>
                  {call.status === "pending" && (
                    <span className="cx-row-actions" onClick={(event) => event.stopPropagation()}>
                      <button
                        type="button"
                        className="btn btn-primary"
                        disabled={busy}
                        aria-label={`Approve ${call.tool}`}
                        onClick={() => void decide(call.id, true)}
                      >
                        Approve
                      </button>
                      <button
                        type="button"
                        className="btn"
                        disabled={busy}
                        aria-label={`Deny ${call.tool}`}
                        onClick={() => void decide(call.id, false)}
                      >
                        Deny
                      </button>
                    </span>
                  )}
                </td>
              </tr>
            );
          })}
        </tbody>
      </table>
      {error && (
        <p className="cx-status" role="alert">
          {error}
        </p>
      )}
    </>
  );
}
