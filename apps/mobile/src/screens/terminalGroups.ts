import { TerminalKind, type Terminal } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import type { SwipePane } from "@puppet-master/client-core/state/sessionLayout";

export type PaneEntry =
  | { kind: "terminal"; paneId: string; title: string; terminal: Terminal }
  | { kind: "page"; paneId: string; title: string };

export interface TerminalGroups {
  agent: Terminal[];
  panes: PaneEntry[];
  tabs: Terminal[];
}

/** A session's terminals as the Terminals list shows them: the agent, its
 * panes in swipe order, and shell tabs, which are every other terminal. */
export function terminalGroups(terminals: readonly Terminal[], panes: readonly SwipePane[]): TerminalGroups {
  const byId = new Map(terminals.map((terminal) => [terminal.id.toString(), terminal]));
  const inPanes = new Set<string>();
  const paneEntries: PaneEntry[] = [];
  for (const pane of panes) {
    if (pane.kind === "page") {
      paneEntries.push({ kind: "page", paneId: pane.paneId, title: pane.title });
    } else if (pane.kind === "terminal") {
      const terminal = byId.get(pane.terminalId);
      if (!terminal) continue;
      inPanes.add(pane.terminalId);
      paneEntries.push({ kind: "terminal", paneId: pane.paneId, title: pane.title, terminal });
    }
  }
  return {
    agent: terminals.filter((t) => t.kind === TerminalKind.AGENT),
    panes: paneEntries,
    tabs: terminals.filter((t) => t.kind !== TerminalKind.AGENT && !inPanes.has(t.id.toString())),
  };
}
