import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { TerminalKind, TerminalSchema } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { terminalGroups } from "./terminalGroups";

describe("the Terminals list", () => {
  it("groups the agent, its panes in swipe order, and the remaining shells as tabs", () => {
    const agent = create(TerminalSchema, { id: 1n, kind: TerminalKind.AGENT });
    const pane = create(TerminalSchema, { id: 2n, kind: TerminalKind.SHELL });
    const tab = create(TerminalSchema, { id: 3n, kind: TerminalKind.SHELL });
    const groups = terminalGroups([agent, pane, tab], [
      { kind: "agent" },
      { kind: "terminal", paneId: "shell-1", terminalId: "2", title: "vim" },
      { kind: "page", paneId: "preview", pageToken: "t", version: 1, title: "desktop-preview" },
      { kind: "terminal", paneId: "gone", terminalId: "9", title: "missing" },
    ]);
    expect(groups.agent).toEqual([agent]);
    expect(groups.panes.map((entry) => [entry.kind, entry.paneId, entry.title])).toEqual([
      ["terminal", "shell-1", "vim"],
      ["page", "preview", "desktop-preview"],
    ]);
    expect(groups.tabs).toEqual([tab]);
  });
});
