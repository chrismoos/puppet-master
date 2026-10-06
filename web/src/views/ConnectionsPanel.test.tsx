// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import {
  blankConnection,
  type Connection,
  type PolicyDraft,
} from "../api/connections";
import * as api from "../api/connections";
import {
  ConnectedEditor,
  ConnectionEditor,
  PolicyReview,
} from "./ConnectionsPanel";

vi.mock("../state/hooks", () => ({
  useAppState: () => ({
    projects: new Map([["1", { id: 1n, name: "bladerun" }]]),
    sessions: new Map(),
  }),
}));

let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.restoreAllMocks();
});

const PAGE = 25;

function button(text: string): HTMLButtonElement {
  return [...host.querySelectorAll("button")].find(
    (candidate) => candidate.textContent?.trim() === text,
  )!;
}

function tab(name: string): HTMLButtonElement {
  return [...host.querySelectorAll<HTMLButtonElement>('[role="tab"]')].find((candidate) =>
    candidate.textContent!.includes(name),
  )!;
}

function type(input: HTMLInputElement, value: string) {
  act(() => {
    Object.getOwnPropertyDescriptor(HTMLInputElement.prototype, "value")!.set!.call(input, value);
    input.dispatchEvent(new Event("input", { bubbles: true }));
  });
}

function draft(tools: Connection["tools"], patch: Partial<Connection> = {}): Connection {
  return {
    id: 1,
    revision: 1,
    config: blankConnection(1),
    active: false,
    created_by_session: 1,
    credential_set: false,
    tested_revision: null,
    policy_proposal: null,
    tools,
    ...patch,
  };
}

function editor(connection: Connection | undefined, layout: "page" | "panel" = "page") {
  return (
    <ConnectionEditor
      connection={connection}
      projectId={1}
      layout={layout}
      sessionId="1"
      onSaved={() => {}}
      onClose={() => {}}
    />
  );
}

const discovered: Connection["tools"] = [
  { name: "read_wiki_structure", description: "List topics", input_schema: {}, suggested_access: "read" },
  { name: "archive_order", description: "", input_schema: {}, suggested_access: "write" },
  { name: "mystery", description: "", input_schema: {}, suggested_access: "unknown" },
];

it("pages large policy proposals and filters changes without losing classification or default differences", () => {
  const connection = draft(
    Array.from({ length: 125 }, (_, index) => ({
      name: `operation${String(index).padStart(3, "0")}`,
      description: "",
      input_schema: {},
      suggested_access: "read" as const,
    })),
  );
  const policy: PolicyDraft = {
    read_policy: "allow",
    write_policy: "deny",
    unknown_policy: "approve",
    rules: Object.fromEntries(
      connection.tools.map((tool) => [tool.name, { access: "read" }]),
    ),
  };
  act(() =>
    root.render(<PolicyReview connection={connection} policy={policy} />),
  );
  expect(host.querySelectorAll("tbody tr")).toHaveLength(PAGE);
  expect(host.querySelector("tbody tr")!.textContent).toBe(
    "operation000Unclassified Require approval · DefaultRead Always allow · Default",
  );
  expect(host.textContent).toContain("Deny Previously require approval");
  expect(host.textContent).toContain("1–25 of 125 tool changes");
  act(() => button("Next").click());
  expect(host.querySelector("tbody tr")!.textContent).toContain("operation025");
  type(host.querySelector("input")!, "124");
  expect(host.querySelectorAll("tbody tr")).toHaveLength(1);
  expect(host.querySelector("tbody tr")!.textContent).toContain("operation124");
  expect(host.textContent).toContain("1–1 of 1 tool changes");
});

it("shows an explicit override change even when today's effective permission is unchanged", () => {
  const connection = draft(
    [{ name: "read", description: "", input_schema: {}, suggested_access: "read" }],
    { active: true, tested_revision: 1 },
  );
  connection.config.rules = { read: { access: "read" } };
  const policy: PolicyDraft = {
    ...connection.config,
    rules: { read: { access: "read", policy: "allow" } },
  };
  act(() =>
    root.render(<PolicyReview connection={connection} policy={policy} />),
  );
  expect(host.querySelectorAll("tbody tr")).toHaveLength(1);
  expect(host.querySelector("tbody tr")!.textContent).toBe(
    "readRead Always allow · DefaultRead Always allow · Override",
  );
});

it("keeps policy proposals paired with the same full connection snapshot while a refresh is pending", async () => {
  const policy: PolicyDraft = {
    read_policy: "allow",
    write_policy: "approve",
    unknown_policy: "approve",
    rules: {},
  };
  const first = draft([], {
    created_by_session: null,
    tested_revision: 1,
    policy_proposal: {
      id: "first",
      revision: 1,
      session_id: 1,
      policy,
      explanation: "First reviewed proposal",
    },
  });
  const second: Connection = {
    ...first,
    revision: 2,
    policy_proposal: {
      id: "second",
      revision: 2,
      session_id: 1,
      policy: { ...policy, write_policy: "allow" },
      explanation: "Replacement proposal",
    },
  };
  let complete!: (value: Connection) => void;
  const pending = new Promise<Connection>((resolve) => {
    complete = resolve;
  });
  vi.spyOn(api, "connectionRequest")
    .mockResolvedValueOnce(first)
    .mockReturnValueOnce(pending);
  const onSaved = vi.fn();
  const onClose = vi.fn();
  await act(async () => {
    root.render(
      <ConnectedEditor connection={first} onSaved={onSaved} onClose={onClose} />,
    );
  });
  expect(host.textContent).toContain("First reviewed proposal");
  await act(async () => {
    root.render(
      <ConnectedEditor connection={second} onSaved={onSaved} onClose={onClose} />,
    );
  });
  expect(host.textContent).toContain("First reviewed proposal");
  expect(host.textContent).not.toContain("Replacement proposal");
  await act(async () => {
    complete(second);
  });
  // Nothing was typed here, so the replacement is taken as it arrives and can be applied.
  expect(host.textContent).toContain("Replacement proposal");
  expect(host.textContent).not.toContain("Reload draft");
  expect(
    host.querySelector<HTMLButtonElement>('button[aria-label="Apply proposed policy"]')!.disabled,
  ).toBe(false);
});

it("holds a change from elsewhere behind a prompt while there are unsaved edits here", () => {
  const first = draft(discovered, { tested_revision: 1 });
  act(() => root.render(editor(first)));
  const writes = host.querySelector<HTMLSelectElement>('select[aria-label="Writes"]')!;
  act(() => {
    writes.value = "deny";
    writes.dispatchEvent(new Event("change", { bubbles: true }));
  });
  const renamed = { ...first, revision: 2, tested_revision: 2, config: { ...first.config, name: "Renamed elsewhere" } };
  act(() => root.render(editor(renamed)));
  expect(host.querySelector('[role="alert"]')!.textContent).toContain("changed this draft");
  expect(host.querySelector<HTMLSelectElement>('select[aria-label="Writes"]')!.value).toBe("deny");
  expect(button("Save draft").disabled).toBe(true);
  act(() => button("Reload draft (discard local edits)").click());
  expect(host.querySelector(".cx-detail-head h2")!.textContent).toBe("Set up Renamed elsewhere");
  expect(host.querySelector<HTMLSelectElement>('select[aria-label="Writes"]')!.value).toBe("approve");
});

it("opens an agent's proposal on the policy tab and copies it into the editor to customize", () => {
  const connection = draft([], {
    policy_proposal: {
      id: "review",
      revision: 1,
      session_id: 1,
      explanation: "Allow reads and approve writes.",
      policy: {
        read_policy: "allow",
        write_policy: "deny",
        unknown_policy: "approve",
        rules: {},
      },
    },
  });
  act(() => root.render(editor(connection)));
  expect(host.querySelector('[role="tab"][aria-selected="true"]')!.textContent).toContain("Policy");
  expect(host.textContent).toContain("Allow reads and approve writes.");
  const writes = () => host.querySelector<HTMLSelectElement>('select[aria-label="Writes"]')!.value;
  expect(writes()).toBe("approve");
  act(() =>
    host.querySelector<HTMLButtonElement>('button[aria-label="Edit proposed policy"]')!.click(),
  );
  expect(writes()).toBe("deny");
  expect(
    host.querySelector<HTMLButtonElement>('button[aria-label="Apply proposed policy"]')!.disabled,
  ).toBe(true);
  expect(host.textContent).toContain("unsaved changes");
});

it("shows one section at a time and switches between them from the tabs", () => {
  act(() => root.render(editor(draft(discovered, { tested_revision: 1 }))));
  expect(host.querySelector('[role="tabpanel"]')!.getAttribute("aria-label")).toBe("Policy");
  expect(host.querySelector('input[aria-label="Filter connection tools"]')).not.toBeNull();
  act(() => tab("Service").click());
  expect(host.querySelector('[role="tabpanel"]')!.getAttribute("aria-label")).toBe("Service");
  expect(host.querySelector('input[aria-label="Filter connection tools"]')).toBeNull();
  expect(host.querySelector('select[aria-label="Project"]')).not.toBeNull();
  act(() => tab("Credentials and test").click());
  expect(host.textContent).toContain("Test passed");
  expect(host.textContent).toContain("3 tools discovered");
  expect(host.textContent).toContain("No credentials needed");
});

it("marks each tab with what is done and what still needs a decision", () => {
  act(() => root.render(editor(draft(discovered, { tested_revision: 1 }))));
  expect(tab("Service").textContent).toBe("✓Service MCP server · bladerun");
  expect(tab("Credentials and test").textContent).toBe("✓Credentials and test Passed · 3 tools");
  expect(tab("Policy").textContent).toBe("3Policy 3 unclassified");
});

it("starts a new connection on the service tab and an untested draft on credentials", () => {
  act(() => root.render(editor(undefined)));
  expect(host.querySelector('[role="tabpanel"]')!.getAttribute("aria-label")).toBe("Service");
  expect(host.textContent).toContain("Add connection");
  act(() => root.render(<div />));
  act(() => root.render(editor(draft([]))));
  expect(host.querySelector('[role="tabpanel"]')!.getAttribute("aria-label")).toBe(
    "Credentials and test",
  );
  expect(host.textContent).toContain("Not tested yet");
  expect(button("Activate connection").disabled).toBe(true);
});

it("explains the empty policy tab and discovers tools from it", async () => {
  const request = vi
    .spyOn(api, "connectionRequest")
    .mockResolvedValue({ ...draft(discovered), tested_revision: 1 });
  act(() => root.render(editor(draft([]))));
  act(() => tab("Policy").click());
  const note = host.querySelector('[role="note"]')!;
  expect(note.textContent).toContain("No tools discovered yet");
  expect(host.querySelector('input[aria-label="Filter connection tools"]')).toBeNull();
  await act(async () => note.querySelector("button")!.click());
  expect(request).toHaveBeenCalledWith("/api/connections/1/test", {});
  expect(host.querySelector('[role="note"]')).toBeNull();
  expect(
    host.querySelector('select[aria-label="Classification for read_wiki_structure"]'),
  ).not.toBeNull();
});

it("opens the policy table on unclassified tools and classifies them from suggestions", () => {
  const connection = draft(discovered, { tested_revision: 1 });
  connection.config.rules = { read_wiki_structure: { access: "read" } };
  act(() => root.render(editor(connection)));
  const rows = () => [...host.querySelectorAll("tr.connection-tool")].map((row) => row.querySelector(".cx-name")!.textContent);
  expect(host.querySelector('.cx-chip[aria-pressed="true"]')!.textContent).toBe("Unclassified2");
  expect(rows()).toEqual(["archive_order", "mystery"]);
  expect(host.textContent).toContain("1–2 of 2 unclassified");
  act(() => button("Use suggestions for all 2").click());
  // The tool with no suggestion stays for a decision of its own.
  expect(rows()).toEqual(["mystery"]);
  expect(host.querySelector('.cx-chip[aria-pressed="true"]')!.textContent).toBe("Unclassified1");
  act(() =>
    [...host.querySelectorAll<HTMLButtonElement>(".cx-chip")]
      .find((chip) => chip.textContent!.startsWith("Write"))!
      .click(),
  );
  expect(rows()).toEqual(["archive_order"]);
});

it("offers authentication as a row of options on the page and a list in the panel", () => {
  const stored = draft([], { credential_set: true, credential_mode: "bearer" });
  act(() => root.render(editor(stored)));
  const pressed = host.querySelector('.cx-seg button[aria-pressed="true"]')!;
  expect(pressed.textContent).toBe("Bearer token");
  expect(host.querySelector('select[aria-label="Authentication"]')).toBeNull();
  expect(host.textContent).toContain("Stored. Leave as is to keep it");
  act(() => root.render(<div />));
  act(() => root.render(editor(stored, "panel")));
  expect(host.querySelector<HTMLSelectElement>('select[aria-label="Authentication"]')!.value).toBe(
    "bearer",
  );
  expect(host.querySelector(".cx-seg")).toBeNull();
});

it("keeps the stored credential unless a new one is typed, and sends the new one when it is", async () => {
  const stored = draft([], { credential_set: true, credential_mode: "bearer" });
  const request = vi.spyOn(api, "connectionRequest").mockResolvedValue(stored);
  act(() => root.render(editor(stored)));
  await act(async () => button("Save draft").click());
  expect(request).toHaveBeenLastCalledWith(
    "/api/connections/1",
    expect.objectContaining({ credential: undefined }),
    "PUT",
  );
  type(host.querySelector<HTMLInputElement>('input[aria-label="Token"]')!, "replacement");
  await act(async () => button("Save draft").click());
  expect(request).toHaveBeenLastCalledWith(
    "/api/connections/1",
    expect.objectContaining({ credential: { mode: "bearer", token: "replacement" } }),
    "PUT",
  );
});

it("lays the panel out with compact tabs, a link to the full page, and no request box for the agent", () => {
  act(() => root.render(editor(draft(discovered, { tested_revision: 1 }), "panel")));
  expect(tab("Credentials").textContent).toContain("Credentials");
  expect(tab("Credentials").textContent).not.toContain("Credentials and test");
  expect(host.querySelector<HTMLAnchorElement>('a[title="Open the full setup page"]')!.getAttribute("href")).toBe(
    "#/settings/connections/1/setup",
  );
  expect(host.textContent).toContain("Prepared by this session's agent.");
  expect(host.querySelectorAll("thead th")).toHaveLength(3);
  expect(host.textContent).not.toContain("Ask the agent");
  expect(button("Activate").disabled).toBe(false);
});
