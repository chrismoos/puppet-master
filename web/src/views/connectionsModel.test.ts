import { describe, expect, it } from "vitest";
import {
  blankConnection,
  type Connection,
  type ConnectionCall,
  type ConnectionTool,
} from "../api/connections";
import {
  agoLabel,
  CALL_STALLED_AFTER_MS,
  callMatches,
  callStatus,
  callTimeline,
  policyLine,
  proposalLine,
  proposalSummary,
  updateFieldChanges,
  setupTabStates,
  shortEndpoint,
  toolCounts,
  toolMatches,
  toolSummary,
  untilLabel,
  withSuggestions,
} from "./connectionsModel";

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;

function tool(name: string, patch: Partial<ConnectionTool> = {}): ConnectionTool {
  return { name, description: "", input_schema: {}, suggested_access: "unknown", ...patch };
}

function connection(patch: Partial<Connection> = {}): Connection {
  return {
    id: 1,
    revision: 3,
    config: blankConnection(1),
    active: false,
    created_by_session: null,
    credential_set: false,
    tested_revision: null,
    tools: [],
    policy_proposal: null,
    ...patch,
  };
}

function call(patch: Partial<ConnectionCall> = {}): ConnectionCall {
  return {
    id: "c1",
    session_id: 7,
    connection_id: 1,
    tool: "update_order",
    arguments: {},
    justification: "Rename the order",
    status: "pending",
    created_at: 1_000_000,
    expires_at: 1_000_000 + 24 * HOUR,
    result: null,
    error: null,
    requires_approval: true,
    ...patch,
  };
}

describe("times", () => {
  it("says how long ago in the coarsest unit that fits", () => {
    expect(agoLabel(0, 12_000)).toBe("12s ago");
    expect(agoLabel(0, 2 * MINUTE + 30_000)).toBe("2m ago");
    expect(agoLabel(0, 3 * HOUR)).toBe("3h ago");
    expect(agoLabel(0, 50 * HOUR)).toBe("2d ago");
  });

  it("says how long until a moment, or nothing once it has passed", () => {
    expect(untilLabel(24 * HOUR, 2 * MINUTE)).toBe("in 23h 58m");
    expect(untilLabel(10 * MINUTE, 0)).toBe("in 10m");
    expect(untilLabel(0, 1)).toBeNull();
  });
});

describe("calls", () => {
  it("labels each status the way the tables show it", () => {
    expect(callStatus("pending")).toEqual({ label: "needs approval", tone: "pending" });
    expect(callStatus("executing")).toEqual({ label: "running", tone: "info" });
    expect(callStatus("outcome_unknown").tone).toBe("bad");
    expect(callStatus("something-new")).toEqual({ label: "something-new", tone: "muted" });
  });

  it("filters by status and by tool, justification, status label or session", () => {
    const name = (id: number) => (id === 7 ? "checkout-fix" : "other");
    expect(callMatches(call(), "", "all", name)).toBe(true);
    expect(callMatches(call(), "", "succeeded", name)).toBe(false);
    expect(callMatches(call(), "needs approval", "pending", name)).toBe(true);
    expect(callMatches(call(), "CHECKOUT", "all", name)).toBe(true);
    expect(callMatches(call(), "rename", "all", name)).toBe(true);
    expect(callMatches(call(), "archive", "all", name)).toBe(false);
  });

  it("tells a held call's story in order, including the stall notice", () => {
    const created = 1_000_000;
    const waiting = callTimeline(call(), created + CALL_STALLED_AFTER_MS + 1);
    expect(waiting.map((entry) => entry.text)).toEqual([
      "Requested, waiting for approval",
      "Session told the call has stalled (5 min without a decision)",
    ]);
    expect(waiting[1]).toMatchObject({ at: created + CALL_STALLED_AFTER_MS, aside: true });

    const settled = callTimeline(
      call({
        status: "succeeded",
        decided_by: "testuser",
        decided_at: created + MINUTE,
        finished_at: created + MINUTE + 900,
      }),
      created + HOUR,
    );
    expect(settled.map((entry) => entry.text)).toEqual([
      "Requested, waiting for approval",
      "Approved by testuser",
      "Succeeded",
    ]);
  });

  it("tells an allowed or a denied call's story without an approval it never had", () => {
    const allowed = callTimeline(
      call({ status: "failed", requires_approval: false, finished_at: 1_000_500, error: "Refused" }),
      2_000_000,
    );
    expect(allowed.map((entry) => entry.text)).toEqual([
      "Requested, allowed by policy",
      "Failed: Refused",
    ]);
    const denied = callTimeline(
      call({ status: "denied", decided_by: "testuser", decided_at: 1_030_000 }),
      2_000_000,
    );
    expect(denied.map((entry) => entry.text)).toEqual([
      "Requested, waiting for approval",
      "Denied by testuser",
    ]);
  });
});

describe("tools and policy", () => {
  const tools = [
    tool("read_wiki", { description: "View documentation", suggested_access: "read" }),
    tool("archive_order", {
      suggested_access: "write",
      operation: { method: "post", path: "/orders/{id}/archive" },
    }),
    tool("ask", {}),
  ];
  const config = {
    ...blankConnection(1),
    rules: { read_wiki: { access: "read" as const }, ask: { access: "read" as const, policy: "approve" as const } },
  };

  it("describes a tool by its description, or by the operation behind it", () => {
    expect(toolSummary(tools[0])).toBe("View documentation");
    expect(toolSummary(tools[1])).toBe("POST /orders/{id}/archive");
    expect(toolSummary(tools[2])).toBe("");
  });

  it("counts tools by effective policy and by classification", () => {
    expect(toolCounts(tools, config)).toEqual({
      all: 3,
      allow: 1,
      approve: 2,
      deny: 0,
      read: 2,
      write: 0,
      unknown: 1,
    });
  });

  it("filters by text, by effective policy, and by classification", () => {
    const names = (query: string, filter: Parameters<typeof toolMatches>[3]) =>
      tools.filter((candidate) => toolMatches(candidate, config, query, filter)).map((t) => t.name);
    expect(names("", "all")).toEqual(["read_wiki", "archive_order", "ask"]);
    expect(names("", "approve")).toEqual(["archive_order", "ask"]);
    expect(names("", "unknown")).toEqual(["archive_order"]);
    expect(names("", "read")).toEqual(["read_wiki", "ask"]);
    expect(names("ARCHIVE", "all")).toEqual(["archive_order"]);
    expect(names("documentation", "allow")).toEqual(["read_wiki"]);
  });

  it("accepts suggestions only for unclassified tools that have one", () => {
    expect(withSuggestions(tools, config.rules)).toEqual({
      read_wiki: { access: "read" },
      ask: { access: "read", policy: "approve" },
      archive_order: { access: "write" },
    });
    expect(withSuggestions([tool("mystery")], {})).toEqual({});
  });

  it("says how policy treats one tool and whether an override decides it", () => {
    expect(policyLine(config, "ask")).toEqual({ text: "read · approval required", override: true });
    expect(policyLine(config, "read_wiki")).toEqual({ text: "read · allow", override: false });
    expect(policyLine(config, "archive_order")).toEqual({
      text: "unclassified · approval required",
      override: false,
    });
  });

  it("summarizes a proposal by its one change, or by how many it makes", () => {
    const current = connection({ config, tools });
    expect(
      proposalSummary(current, {
        ...config,
        rules: { ...config.rules, read_wiki: { access: "read", policy: "approve" } },
      }),
    ).toBe("read_wiki: allow → approval required.");
    expect(
      proposalSummary(current, {
        ...config,
        write_policy: "deny",
        rules: { read_wiki: { access: "write" }, archive_order: { access: "write" } },
      }),
    ).toBe("3 tool changes, 1 default change.");
    expect(proposalSummary(current, config)).toBe("No changes to the current policy.");
  });

  it("lists the setup fields an update changes, showing values only where a pair is readable", () => {
    const changes = {
      fields: [
        { field: "name" as const, from: "orders", to: "orders v2" },
        { field: "project_id" as const, from: 1, to: 2 },
        { field: "oauth" as const, from: null, to: { client_id: "pm" } },
        { field: "schema" as const },
      ],
      tools: { added: [], removed: [], changed: [] },
      requires_activation: true,
    };
    expect(updateFieldChanges(changes, (id) => `Project ${id}`)).toEqual([
      { label: "Name", from: "orders", to: "orders v2" },
      { label: "Project", from: "Project 1", to: "Project 2" },
      { label: "OAuth settings", from: "None", to: "Changed" },
      { label: "OpenAPI document" },
    ]);
  });

  it("summarizes an update by its setup and tool changes, counting policy against the proposed tools", () => {
    const current = connection({ config, tools });
    const proposal = {
      id: "p",
      revision: 1,
      session_id: 7,
      explanation: "",
      policy: { ...config, rules: { ...config.rules, lookup: { access: "read" as const } } },
      changes: {
        fields: [{ field: "name" as const, from: "a", to: "b" }, { field: "schema" as const }],
        tools: {
          added: [{ name: "lookup", description: "", suggested_access: "read" as const }],
          removed: ["mystery"],
          changed: ["ask", "read_wiki"],
        },
        requires_activation: false,
      },
    };
    expect(proposalLine(current, proposal)).toBe(
      "1 setting changed, 1 tool added, 1 tool removed, 2 tools changed. Policy: lookup: approval required → allow.",
    );
    expect(proposalLine(current, { ...proposal, changes: null })).toBe(
      "No changes to the current policy.",
    );
  });
});

describe("setup tabs", () => {
  it("numbers the steps of a connection nothing has been saved for", () => {
    const tabs = setupTabStates(undefined, blankConnection(1), "bladerun");
    expect(tabs.service).toEqual({ mark: { kind: "step", step: 1 }, summary: "MCP server · bladerun" });
    expect(tabs.credentials).toEqual({ mark: { kind: "step", step: 2 }, summary: "Not tested" });
    expect(tabs.policy).toEqual({ mark: { kind: "step", step: 3 }, summary: "No tools yet" });
  });

  it("checks what is done and counts what still needs a decision", () => {
    const tools = [tool("a"), tool("b"), tool("c")];
    const config = { ...blankConnection(1), kind: "openapi" as const, rules: { a: { access: "read" as const } } };
    const tested = connection({ config, tools, tested_revision: 3 });
    const tabs = setupTabStates(tested, config, "bladerun");
    expect(tabs.service).toEqual({ mark: { kind: "done" }, summary: "REST API · bladerun" });
    expect(tabs.credentials).toEqual({ mark: { kind: "done" }, summary: "Passed · 3 tools" });
    expect(tabs.policy).toEqual({ mark: { kind: "todo", count: 2 }, summary: "2 unclassified" });

    const classified = { ...config, rules: { a: { access: "read" as const }, b: { access: "read" as const }, c: { access: "write" as const } } };
    expect(setupTabStates(tested, classified, "bladerun").policy).toEqual({
      mark: { kind: "done" },
      summary: "All classified",
    });
    expect(
      setupTabStates(connection({ config, tools, tested_revision: 2 }), config, "bladerun").credentials,
    ).toEqual({ mark: { kind: "step", step: 2 }, summary: "Changed since the last test" });
  });
});

describe("shortEndpoint", () => {
  it("keeps host and path and drops scheme, query and credentials", () => {
    expect(shortEndpoint("https://mcp.deepwiki.com/mcp")).toBe("mcp.deepwiki.com/mcp");
    expect(shortEndpoint("https://user:pw@api.example.com/?key=1")).toBe("api.example.com");
    expect(shortEndpoint("not a url")).toBe("not a url");
  });
});
