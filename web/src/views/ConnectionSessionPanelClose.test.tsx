// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import * as api from "../api/connections";
import { blankConnection, type Connection } from "../api/connections";
import { ConnectionSessionPanel } from "./ConnectionsPanel";

vi.mock("../state/hooks", () => ({ useAppState: () => ({ projects: new Map(), sessions: new Map() }) }));

const draft: Connection = {
  id: 1,
  revision: 2,
  config: { ...blankConnection(1), name: "Orders API", endpoint: "https://orders.example.com" },
  active: false,
  created_by_session: 7,
  credential_set: true,
  tested_revision: null,
  tools: [],
  policy_proposal: null,
};

let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  vi.useFakeTimers();
  // Node's own localStorage global shadows jsdom's and has no methods unless
  // the process was started with a storage file.
  const stored = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => stored.get(key) ?? null,
    setItem: (key: string, value: string) => void stored.set(key, value),
    removeItem: (key: string) => void stored.delete(key),
  });
  vi.spyOn(api, "listConnections").mockResolvedValue([draft]);
  vi.spyOn(api, "listConnectionCalls").mockResolvedValue([]);
  vi.spyOn(api, "connectionRequest").mockResolvedValue(draft as never);
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.useRealTimers();
  vi.restoreAllMocks();
  vi.unstubAllGlobals();
});

it("closes the whole setup sidebar and keeps it closed through background refreshes", async () => {
  await act(async () => root.render(<ConnectionSessionPanel sessionId="7" />));
  await act(async () => {});
  expect(host.querySelector("aside.connection-session-panel")).not.toBeNull();
  const close = host.querySelector<HTMLButtonElement>('button[aria-label="Close"]')!;
  await act(async () => close.click());
  expect(host.querySelector("aside")).toBeNull();
  // The panel polls every two seconds; the same draft must not reopen it.
  await act(async () => vi.advanceTimersByTimeAsync(6_000));
  expect(api.listConnections).toHaveBeenCalledTimes(4);
  expect(host.querySelector("aside")).toBeNull();
  // A new proposal from the agent is new work, and opens it again.
  vi.mocked(api.listConnections).mockResolvedValue([
    { ...draft, policy_proposal: { id: "p1", revision: 2, session_id: 7, explanation: "", policy: { read_policy: "allow", write_policy: "approve", unknown_policy: "approve", rules: {} } } },
  ]);
  await act(async () => vi.advanceTimersByTimeAsync(2_000));
  expect(host.querySelector("aside.connection-session-panel")).not.toBeNull();
});

it("keeps a closed draft closed after a reload, until the agent brings new work", async () => {
  await act(async () => root.render(<ConnectionSessionPanel sessionId="7" />));
  await act(async () => {});
  const close = host.querySelector<HTMLButtonElement>('button[aria-label="Close"]')!;
  await act(async () => close.click());
  expect(host.querySelector("aside")).toBeNull();
  // A reload mounts a fresh panel over the same browser storage.
  act(() => root.unmount());
  root = createRoot(host);
  await act(async () => root.render(<ConnectionSessionPanel sessionId="7" />));
  await act(async () => {});
  expect(host.querySelector("aside")).toBeNull();
  vi.mocked(api.listConnections).mockResolvedValue([
    { ...draft, policy_proposal: { id: "p2", revision: 2, session_id: 7, explanation: "", policy: { read_policy: "allow", write_policy: "approve", unknown_policy: "approve", rules: {} } } },
  ]);
  await act(async () => vi.advanceTimersByTimeAsync(2_000));
  expect(host.querySelector("aside.connection-session-panel")).not.toBeNull();
});

const proposal = {
  id: "p1",
  revision: 2,
  session_id: 7,
  explanation: "Allow reads",
  policy: { read_policy: "allow" as const, write_policy: "approve" as const, unknown_policy: "approve" as const, rules: {} },
};

async function applyProposal(listed: Connection, answer: () => Promise<unknown>) {
  vi.mocked(api.listConnections).mockResolvedValue([listed]);
  vi.mocked(api.connectionRequest).mockImplementation(async (path: string) =>
    (path.endsWith("/policy") ? answer() : listed) as never,
  );
  await act(async () => root.render(<ConnectionSessionPanel sessionId="7" />));
  await act(async () => {});
  const apply = host.querySelector<HTMLButtonElement>('button[aria-label="Apply proposed policy"]')!;
  await act(async () => apply.click());
  await act(async () => {});
}

it("closes the whole sidebar when a decision completes setup, and it stays closed", async () => {
  const active = { ...draft, active: true, tested_revision: 2, policy_proposal: proposal };
  const settled = { ...active, revision: 3, tested_revision: 3, policy_proposal: null };
  await applyProposal(active, async () => settled);
  expect(host.querySelector("aside")).toBeNull();
  vi.mocked(api.listConnections).mockResolvedValue([settled]);
  await act(async () => vi.advanceTimersByTimeAsync(4_000));
  expect(host.querySelector("aside")).toBeNull();
});

it("keeps the sidebar open on an intermediate step of an unfinished draft", async () => {
  const unfinished = { ...draft, policy_proposal: proposal };
  await applyProposal(unfinished, async () => ({ ...unfinished, revision: 3, policy_proposal: null }));
  expect(host.querySelector("aside.connection-session-panel")).not.toBeNull();
  expect(host.textContent).toContain("Policy applied.");
});

it("keeps the sidebar open with the error when completing fails", async () => {
  const active = { ...draft, active: true, tested_revision: 2, policy_proposal: proposal };
  await applyProposal(active, async () => {
    throw new Error("Connection changed. Reload the draft.");
  });
  expect(host.querySelector("aside.connection-session-panel")).not.toBeNull();
  expect(host.querySelector("[role=alert]")?.textContent).toContain("Connection changed");
});

it("widens when its left edge is dragged, and remembers the width", async () => {
  localStorage.removeItem("pm.connectionPanelWidth");
  await act(async () => root.render(<ConnectionSessionPanel sessionId="7" />));
  await act(async () => {});
  const panel = host.querySelector<HTMLElement>("aside.connection-session-panel")!;
  expect(panel.style.getPropertyValue("--connection-panel-w")).toBe("440px");
  panel.getBoundingClientRect = () => ({ right: 1200 }) as DOMRect;
  const handle = panel.querySelector<HTMLElement>('[role="separator"]')!;
  const pointer = (type: string, clientX: number) => {
    const event = new Event(type, { bubbles: true, cancelable: true });
    Object.assign(event, { clientX });
    return event;
  };
  act(() => {
    handle.dispatchEvent(pointer("pointerdown", 760));
  });
  act(() => {
    window.dispatchEvent(pointer("pointermove", 600));
  });
  expect(panel.style.getPropertyValue("--connection-panel-w")).toBe("600px");
  act(() => {
    window.dispatchEvent(pointer("pointerup", 600));
  });
  expect(localStorage.getItem("pm.connectionPanelWidth")).toBe("600");
});
