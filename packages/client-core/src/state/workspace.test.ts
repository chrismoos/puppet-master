import { describe, expect, it } from "vitest";
import { SessionState, TerminalKind, TerminalRunState, type Session, type Terminal } from "../gen/pm/v1/pm_pb";
import { agentTerminalForSession, assignTerminal, closePane, dropTerminal, movePane, paneDropPositionForRatios, paneDropRegionRect, paneIds, parseWorkspaceLayout, removeTerminalFromLayout, setSplitRatio, splitPane, workspaceTerminalEntries, type WorkspaceLayout, type WorkspaceSplit } from "./workspace";

const root: WorkspaceLayout = { kind: "pane", paneId: "one", terminalId: "7" };

function session(id: bigint, state = SessionState.WORKING, goal = "", taskTitle = `task ${id}`): Session {
  return { id, state, goal, headline: "", taskTitle } as Session;
}

function terminal(
  id: bigint,
  sessionId: bigint,
  kind: TerminalKind,
  state = TerminalRunState.RUNNING,
  title = "",
): Terminal {
  return { id, sessionId, kind, state, title } as Terminal;
}

describe("workspace layout", () => {
  it("resolves the dragged session to its agent terminal", () => {
    const terminals = [
      { id: 8n, sessionId: 1n, kind: TerminalKind.SHELL, state: TerminalRunState.RUNNING },
      { id: 7n, sessionId: 1n, kind: TerminalKind.AGENT, state: TerminalRunState.RUNNING },
      { id: 6n, sessionId: 1n, kind: TerminalKind.AGENT, state: TerminalRunState.EXITED },
    ] as Terminal[];
    expect(agentTerminalForSession(terminals, "1")?.id).toBe(7n);
    expect(agentTerminalForSession(terminals, "1", "7")?.id).toBe(7n);
  });

  it("offers only live terminals from live sessions", () => {
    const sessions = new Map([
      ["1", session(1n)],
      ["2", session(2n, SessionState.EXITED)],
      ["4", session(4n, SessionState.FAILED)],
    ]);
    const terminals = [
      terminal(1n, 1n, TerminalKind.AGENT),
      terminal(2n, 1n, TerminalKind.SHELL, TerminalRunState.STARTING, "build"),
      terminal(3n, 1n, TerminalKind.SHELL, TerminalRunState.EXITED, "old"),
      terminal(4n, 2n, TerminalKind.AGENT),
      terminal(5n, 3n, TerminalKind.SHELL, TerminalRunState.RUNNING, "orphan"),
      terminal(6n, 1n, TerminalKind.UNSPECIFIED),
      terminal(7n, 1n, TerminalKind.SHELL, TerminalRunState.FAILED, "broken"),
      terminal(8n, 4n, TerminalKind.AGENT),
    ];

    expect(workspaceTerminalEntries(terminals, sessions).map((entry) => entry.terminal.id))
      .toEqual([1n, 2n]);
  });

  it("labels agents by goal then task title", () => {
    const sessions = new Map([
      ["1", session(1n, SessionState.WORKING, "Running checks", "Fix tests")],
      ["2", session(2n, SessionState.WORKING, "", "Ship release")],
    ]);
    const entries = workspaceTerminalEntries([
      terminal(2n, 2n, TerminalKind.AGENT),
      terminal(1n, 1n, TerminalKind.AGENT),
    ], sessions);

    expect(entries.map((entry) => entry.label)).toEqual([
      "Running checks · agent",
      "Ship release · agent",
    ]);
  });

  it("labels shells by running command then configured title", () => {
    const sessions = new Map([["1", session(1n, SessionState.WORKING, "Dev server")]]);
    const entries = workspaceTerminalEntries([
      terminal(2n, 1n, TerminalKind.SHELL, TerminalRunState.RUNNING, "tests"),
      terminal(3n, 1n, TerminalKind.SHELL, TerminalRunState.RUNNING, "Shell"),
      terminal(4n, 1n, TerminalKind.SHELL, TerminalRunState.RUNNING, "release logs"),
    ], sessions, new Map([["2", "npm test"]]));

    expect(entries.map((entry) => entry.label)).toEqual([
      "Dev server · npm test",
      "Dev server · shell 3",
      "Dev server · release logs",
    ]);
  });

  it("maps the final pointer position to the visible drop targets", () => {
    expect(paneDropPositionForRatios(0.4, 0.5, true)).toBe("swap");
    expect(paneDropPositionForRatios(0.6, 0.5, true)).toBe("replace");
    expect(paneDropPositionForRatios(0.4, 0.5, false)).toBe("replace");
    expect(paneDropPositionForRatios(0.5, 0.1, true)).toBe("above");
  });

  it("maps drop positions to the highlighted pane region", () => {
    expect(paneDropRegionRect("left")).toEqual({ left: 0, top: 0, width: 50, height: 100 });
    expect(paneDropRegionRect("right")).toEqual({ left: 50, top: 0, width: 50, height: 100 });
    expect(paneDropRegionRect("above")).toEqual({ left: 0, top: 0, width: 100, height: 50 });
    expect(paneDropRegionRect("below")).toEqual({ left: 0, top: 50, width: 100, height: 50 });
    expect(paneDropRegionRect("swap")).toEqual({ left: 0, top: 0, width: 100, height: 100 });
    expect(paneDropRegionRect("replace")).toEqual({ left: 0, top: 0, width: 100, height: 100 });
  });

  it("splits and collapses the tree without losing the surviving pane", () => {
    const split = splitPane(root, "one", "columns", "split", "two");
    expect(paneIds(split)).toEqual(["one", "two"]);
    expect(closePane(split, "one")).toEqual({ kind: "pane", paneId: "two", terminalId: null });
  });

  it("removes a closed terminal with pane-close semantics", () => {
    const split = assignTerminal(splitPane(root, "one", "columns", "split", "two"), "two", "8");
    expect(removeTerminalFromLayout(split, "7")).toEqual({
      kind: "pane", paneId: "two", terminalId: "8",
    });
    expect(removeTerminalFromLayout(root, "7")).toEqual({
      kind: "pane", paneId: "one", terminalId: null,
    });
    expect(removeTerminalFromLayout(split, "99")).toBe(split);
  });

  it("keeps a terminal in only one pane", () => {
    const split = splitPane(root, "one", "rows", "split", "two");
    const assigned = assignTerminal(split, "two", "7");
    expect(assigned).toMatchObject({
      first: { terminalId: null },
      second: { terminalId: "7" },
    });
  });

  it("bounds persisted ratios and rejects malformed layouts", () => {
    const split = splitPane(root, "one", "columns", "split", "two");
    expect((setSplitRatio(split, "split", 1) as WorkspaceSplit).ratio).toBe(0.85);
    expect(parseWorkspaceLayout({ kind: "wat" })).toBeNull();
    expect(parseWorkspaceLayout(split)).toEqual(split);
  });

  it("drops an agent before or after the target pane", () => {
    const right = dropTerminal(root, "one", "8", "right", "split-right", "two");
    expect(right).toMatchObject({
      axis: "columns",
      first: { terminalId: "7" },
      second: { terminalId: "8" },
    });
    const above = dropTerminal(root, "one", "8", "above", "split-above", "three");
    expect(above).toMatchObject({
      axis: "rows",
      first: { terminalId: "8" },
      second: { terminalId: "7" },
    });
  });

  it("moves a terminal instead of showing the same PTY twice", () => {
    const split = dropTerminal(root, "one", "8", "right", "split", "two");
    const moved = dropTerminal(split, "one", "8", "replace", "unused", "unused");
    expect(moved).toMatchObject({
      first: { terminalId: "8" },
      second: { terminalId: null },
    });
  });

  it("moves a pane to a visual edge and collapses its old position", () => {
    const split = splitPane(root, "one", "columns", "split-one", "two");
    const populated = assignTerminal(split, "two", "8");
    expect(movePane(populated, "one", "two", "below", "split-two")).toMatchObject({
      axis: "rows",
      first: { paneId: "two", terminalId: "8" },
      second: { paneId: "one", terminalId: "7" },
    });
  });

  it("swaps pane contents when the swap target is selected", () => {
    const split = assignTerminal(splitPane(root, "one", "columns", "split-one", "two"), "two", "8");
    expect(movePane(split, "one", "two", "swap", "unused")).toMatchObject({
      first: { paneId: "one", terminalId: "8" },
      second: { paneId: "two", terminalId: "7" },
    });
  });

  it("replaces the target and collapses the dragged pane's old slot", () => {
    const split = assignTerminal(splitPane(root, "one", "columns", "split-one", "two"), "two", "8");
    expect(movePane(split, "one", "two", "replace", "unused")).toEqual({
      kind: "pane",
      paneId: "two",
      terminalId: "7",
    });
  });
});
