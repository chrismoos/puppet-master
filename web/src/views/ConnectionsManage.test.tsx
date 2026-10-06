// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { connectionRoutePath } from "@puppet-master/client-core/router";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as api from "../api/connections";
import { blankConnection, type Connection, type ConnectionCall } from "../api/connections";
import { SettingsConnections } from "./ConnectionsManage";

vi.mock("../state/hooks", () => ({
  useAppState: () => ({
    projects: new Map([["1", { id: 1n, name: "puppet-master" }]]),
    sessions: new Map([["797", { id: 797n, goal: "supervisor" }]]),
  }),
}));

const HOUR = 60 * 60 * 1000;
const CALL_ID = "e3c946336a9d8063";

function tool(name: string): Connection["tools"][number] {
  return { name, description: `About ${name}`, input_schema: {}, suggested_access: "read" };
}

const deepwiki: Connection = {
  id: 1,
  revision: 3,
  config: {
    ...blankConnection(1),
    name: "DeepWiki",
    endpoint: "https://mcp.deepwiki.com/mcp",
    rules: {
      ask_wiki_question: { access: "read", policy: "approve" },
      read_wiki_contents: { access: "read" },
      read_wiki_structure: { access: "read" },
    },
  },
  active: true,
  created_by_session: 797,
  credential_set: false,
  tested_revision: 3,
  tools: [],
  tool_count: 3,
  policy_proposal: null,
};
const tracker: Connection = {
  ...deepwiki,
  id: 2,
  config: { ...blankConnection(1), name: "Issue tracker", endpoint: "https://tracker.example/mcp" },
  active: false,
  tested_revision: null,
  tool_count: 0,
};
const full: Connection = {
  ...deepwiki,
  tools: [tool("ask_wiki_question"), tool("read_wiki_contents"), tool("read_wiki_structure")],
};

function call(patch: Partial<ConnectionCall>): ConnectionCall {
  const now = Date.now();
  return {
    id: CALL_ID,
    session_id: 797,
    project_id: 1,
    connection_id: 1,
    connection_revision: 3,
    tool: "ask_wiki_question",
    arguments: null,
    justification: "Five-minute stalled test",
    status: "pending",
    created_at: now - 2 * 60 * 1000,
    expires_at: now + 24 * HOUR,
    result: null,
    error: null,
    requires_approval: true,
    ...patch,
  };
}
const pending = call({});
const succeeded = call({ id: "aaaa", tool: "read_wiki_structure", status: "succeeded", justification: "", requires_approval: false });

let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  vi.spyOn(api, "listConnections").mockResolvedValue([deepwiki, tracker]);
  vi.spyOn(api, "listConnectionCalls").mockResolvedValue([pending, succeeded]);
  vi.spyOn(api, "connectionRequest").mockImplementation(async (path: string) => {
    if (path === `/api/connection-calls/${CALL_ID}`)
      return { ...pending, arguments: { repoName: "tokio-rs/tokio" } } as never;
    return full as never;
  });
  location.hash = "";
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.restoreAllMocks();
});

async function show(view?: Parameters<typeof SettingsConnections>[0]["view"]) {
  await act(async () => root.render(<SettingsConnections view={view} />));
  await act(async () => {});
}

function table(index: number): string[][] {
  return [...host.querySelectorAll("table")[index].querySelectorAll("tbody tr")].map((row) =>
    [...row.querySelectorAll("td")].map((cell) => cell.textContent!.trim()),
  );
}

function button(text: string): HTMLButtonElement {
  return [...host.querySelectorAll("button")].find(
    (candidate) => candidate.textContent?.trim() === text,
  )!;
}

it("lists connections and calls as tables, one line each", async () => {
  await show();
  expect(table(0)).toEqual([
    ["DeepWiki", "MCP", "puppet-master", "mcp.deepwiki.com/mcp", "3", "active"],
    ["Issue tracker", "MCP", "puppet-master", "tracker.example/mcp", "—", "draft"],
  ]);
  expect(table(1)).toEqual([
    ["2m ago", "needs approval", "ask_wiki_question", "DeepWiki", "#797 supervisor", "Five-minute stalled test", "ApproveDeny"],
    ["2m ago", "succeeded", "read_wiki_structure", "DeepWiki", "#797 supervisor", "—", ""],
  ]);
  expect(host.textContent).toContain("1 waiting · 2 most recent");
  expect(host.textContent).not.toContain("Ask the agent");
});

it("gives every connection and every call an address of its own", async () => {
  await show();
  expect(host.querySelector<HTMLAnchorElement>("table a")!.getAttribute("href")).toBe(
    "#/settings/connections/1",
  );
  act(() => host.querySelectorAll<HTMLElement>("table")[1].querySelector<HTMLElement>("tbody tr")!.click());
  expect(location.hash).toBe(`#/settings/connections/calls/${CALL_ID}`);
  act(() => host.querySelector<HTMLElement>("table tbody tr")!.click());
  expect(location.hash).toBe("#/settings/connections/1");
  expect(
    [...host.querySelectorAll("a")].find((a) => a.textContent === "Add connection")!.getAttribute("href"),
  ).toBe("#/settings/connections/new");
});

it("decides a pending call from its row without opening it", async () => {
  await show();
  location.hash = "";
  await act(async () => host.querySelector<HTMLButtonElement>('button[aria-label="Approve ask_wiki_question"]')!.click());
  expect(api.connectionRequest).toHaveBeenCalledWith(`/api/connection-calls/${CALL_ID}/decision`, { approve: true });
  expect(location.hash).toBe("");
});

it("opens a call over the list with its arguments, policy, timeline and decision", async () => {
  await show({ view: "call", callId: CALL_ID });
  const dialog = host.querySelector('[role="dialog"]')!;
  expect(dialog.querySelector("h2")!.textContent).toBe("ask_wiki_question needs approval");
  const facts = [...dialog.querySelectorAll("dd")].map((dd) => dd.textContent!.trim());
  expect(facts[0]).toBe("DeepWiki · mcp.deepwiki.com/mcp · revision 3");
  expect(facts[1]).toBe("Session #797 · supervisor · puppet-master");
  expect(facts[2]).toBe("Five-minute stalled test");
  expect(facts[3]).toBe("read · approval required (tool override)");
  expect(facts[4]).toMatch(/^in 23h 5\dm$/);
  expect(dialog.querySelector(".cx-timeline")!.textContent).toContain("Requested, waiting for approval");
  expect(dialog.querySelectorAll("pre")[0].textContent).toContain('"repoName": "tokio-rs/tokio"');
  expect(dialog.querySelectorAll("pre")[1].textContent).toContain("Nothing yet");
  // The list stays behind the modal.
  expect(host.querySelectorAll("table")).toHaveLength(2);
  await act(async () => [...dialog.querySelectorAll("button")].find((b) => b.textContent === "Deny")!.click());
  expect(api.connectionRequest).toHaveBeenCalledWith(`/api/connection-calls/${CALL_ID}/decision`, { approve: false });
  act(() => [...dialog.querySelectorAll("button")].find((b) => b.textContent === "Close")!.click());
  expect(location.hash).toBe("#/settings/connections");
});

it("shows a connection on a page of its own, with its tools, policy and calls", async () => {
  await show({ view: "detail", id: 1 });
  expect(host.querySelector(".cx-crumbs")!.textContent).toBe("Connections / DeepWiki");
  expect(host.querySelector(".cx-detail-head")!.textContent).toContain("DeepWikiactiveMCP");
  expect([...host.querySelectorAll(".cx-facts dd")].map((dd) => dd.textContent)).toEqual([
    "https://mcp.deepwiki.com/mcp",
    "puppet-master",
    "None needed",
    "Revision 3 · passed",
    "Session #797",
  ]);
  expect(table(0).map((row) => [row[0], row[4]])).toEqual([
    ["ask_wiki_question", "approve"],
    ["read_wiki_contents", "allow"],
    ["read_wiki_structure", "allow"],
  ]);
  expect(host.textContent).toContain("Defaults: reads allow · writes approve · unclassified approve");
  expect(table(1).map((row) => row[2])).toEqual(["ask_wiki_question", "read_wiki_structure"]);
  expect(host.textContent).toContain("1 waiting · 2 total");
  expect(
    [...host.querySelectorAll("a")].find((a) => a.textContent === "Edit setup")!.getAttribute("href"),
  ).toBe("#/settings/connections/1/setup");
  // The connection's own page does not list every connection beside it.
  expect(host.textContent).not.toContain("Issue tracker");
});

it("filters a connection's tools by effective policy and its calls by status", async () => {
  await show({ view: "detail", id: 1 });
  const chip = (label: string) =>
    [...host.querySelectorAll<HTMLButtonElement>(".cx-chip")].find((c) => c.textContent!.startsWith(label))!;
  expect(chip("Approve").textContent).toBe("Approve1");
  act(() => chip("Approve").click());
  expect(table(0).map((row) => row[0])).toEqual(["ask_wiki_question"]);
  expect(host.querySelector(".ui-pager")!.textContent).toContain("1–1 of 1");
  act(() => chip("Succeeded").click());
  expect(table(1).map((row) => row[2])).toEqual(["read_wiki_structure"]);
});

it("saves a tool's classification the moment it changes", async () => {
  await show({ view: "detail", id: 1 });
  const override = host.querySelector<HTMLSelectElement>('select[aria-label="Override for read_wiki_contents"]')!;
  await act(async () => {
    override.value = "deny";
    override.dispatchEvent(new Event("change", { bubbles: true }));
  });
  expect(api.connectionRequest).toHaveBeenCalledWith(
    "/api/connections/1",
    {
      revision: 3,
      config: {
        ...full.config,
        rules: { ...full.config.rules, read_wiki_contents: { access: "read", policy: "deny" } },
      },
    },
    "PUT",
  );
});

it("removes a connection only after the user confirms, then returns to the list", async () => {
  await show({ view: "detail", id: 1 });
  expect(button("Remove")).toBeDefined();
  await act(async () => button("Remove").click());
  const dialog = host.querySelector('[role="dialog"]')!;
  expect(dialog.textContent).toContain("Remove “DeepWiki”?");
  expect(api.connectionRequest).not.toHaveBeenCalledWith(expect.stringMatching(/^\/api\/connections\/1$/), expect.anything(), "DELETE");
  await act(async () => button("cancel").click());
  expect(host.querySelector('[role="dialog"]')).toBeNull();
  await act(async () => button("Remove").click());
  await act(async () => button("remove connection").click());
  await act(async () => {});
  expect(api.connectionRequest).toHaveBeenCalledWith("/api/connections/1", { revision: 3 }, "DELETE");
  expect(location.hash).toBe(`#${connectionRoutePath()}`);
});

it("shows a removed connection as history without its actions", async () => {
  vi.mocked(api.connectionRequest).mockResolvedValue({ ...full, active: false, deleted_at: Date.now() } as never);
  await show({ view: "detail", id: 1 });
  expect(host.textContent).toContain("removed");
  expect(host.textContent).toContain("This connection was removed.");
  expect(button("Remove")).toBeUndefined();
  expect(button("Test connection")).toBeUndefined();
});

it("says so when an address names a connection that is gone", async () => {
  await show({ view: "detail", id: 9 });
  expect(host.querySelector('[role="alert"]')!.textContent).toContain("no longer exists");
  expect(button("Add connection")).toBeUndefined();
});

it("opens setup for an existing connection as a page and closes back to the connection", async () => {
  await show({ view: "setup", id: 1 });
  expect(host.querySelector(".cx-crumbs")!.textContent).toBe("Connections / DeepWiki / Setup");
  expect(host.querySelector(".cx-detail-head h2")!.textContent).toBe("Set up DeepWiki");
  act(() => button("Close").click());
  expect(location.hash).toBe("#/settings/connections/1");
});
