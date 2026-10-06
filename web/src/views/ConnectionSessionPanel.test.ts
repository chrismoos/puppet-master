import { describe, expect, it } from "vitest";
import { blankConnection, type Connection, type ConnectionCall } from "../api/connections";
import { sessionPanelKey, sessionPanelState } from "./ConnectionsPanel";

function connection(id: number, patch: Partial<Connection> = {}): Connection {
  return {
    id,
    revision: 1,
    config: blankConnection(1),
    active: false,
    created_by_session: 7,
    credential_set: false,
    tested_revision: null,
    tools: [],
    policy_proposal: null,
    ...patch,
  };
}

function call(id: string, status = "pending", session_id = 7): ConnectionCall {
  return {
    id,
    session_id,
    connection_id: 1,
    tool: "write",
    arguments: null,
    justification: "",
    status,
    created_at: 0,
    expires_at: 0,
    result: null,
    error: null,
  };
}

const none = new Set<string>();
const base = { sessionId: "7", connections: [] as Connection[], calls: [] as ConnectionCall[], selected: null, dismissed: none, dismissedCalls: none };

describe("session connection panel", () => {
  it("opens on the session's own draft", () => {
    const draft = connection(1);
    expect(sessionPanelState({ ...base, connections: [draft] })?.current).toBe(draft);
  });

  // The reported bug: once closed, the panel kept rendering a list of
  // connection buttons, and the workspace kept a column for it.
  it("renders nothing once its draft is dismissed, even after the draft is saved again", () => {
    const draft = connection(1);
    const dismissed = new Set([sessionPanelKey("7", draft)]);
    expect(sessionPanelState({ ...base, connections: [draft], dismissed })).toBeNull();
    const saved = connection(1, { revision: 4, credential_set: true, tested_revision: 4 });
    expect(sessionPanelState({ ...base, connections: [saved], dismissed })).toBeNull();
  });

  it("reopens for a new proposal or a new draft from the agent", () => {
    const draft = connection(1);
    const dismissed = new Set([sessionPanelKey("7", draft)]);
    const proposed = connection(1, {
      policy_proposal: { id: "p2", revision: 1, session_id: 7, explanation: "", policy: { read_policy: "allow", write_policy: "approve", unknown_policy: "approve", rules: {} } },
    });
    expect(sessionPanelState({ ...base, connections: [proposed], dismissed })?.current).toBe(proposed);
    const another = connection(2);
    expect(sessionPanelState({ ...base, connections: [draft, another], dismissed })?.current).toBe(another);
  });

  it("keeps a deliberately selected connection open across saves", () => {
    const active = connection(1, { active: true });
    expect(sessionPanelState({ ...base, connections: [active] })).toBeNull();
    expect(sessionPanelState({ ...base, connections: [active], selected: 1 })?.current).toBe(active);
  });

  it("shows this session's pending approvals until dismissed, and a new one again", () => {
    const calls = [call("a"), call("b", "succeeded"), call("c", "pending", 9)];
    expect(sessionPanelState({ ...base, calls })?.pending.map((c) => c.id)).toEqual(["a"]);
    const dismissedCalls = new Set(["a"]);
    expect(sessionPanelState({ ...base, calls, dismissedCalls })).toBeNull();
    expect(sessionPanelState({ ...base, calls: [...calls, call("d")], dismissedCalls })?.pending.map((c) => c.id)).toEqual(["d"]);
  });

  it("ignores other sessions' drafts", () => {
    expect(sessionPanelState({ ...base, connections: [connection(1, { created_by_session: 9 })] })).toBeNull();
  });
});
