import { Modal, Pressable, SafeAreaView, ScrollView, StyleSheet, Text, View } from "react-native";

import type { Terminal } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { TerminalKind, TerminalRunState } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { shellTabLabel } from "@puppet-master/client-core/terminal/label";

import type { SwipePane } from "@puppet-master/client-core/state/sessionLayout";

import { colors } from "../theme";
import { terminalGroups, type PaneEntry } from "./terminalGroups";

function runStateLabel(terminal: Terminal): { text: string; color: string } {
  switch (terminal.state) {
    case TerminalRunState.STARTING:
      return { text: "starting", color: colors.slate };
    case TerminalRunState.RUNNING:
      return { text: "running", color: colors.green };
    case TerminalRunState.EXITED:
      return {
        text: terminal.exitCode !== undefined ? `exited (${terminal.exitCode})` : "exited",
        color: terminal.exitCode === 0 ? colors.textMuted : colors.amber,
      };
    case TerminalRunState.FAILED:
      return {
        text: terminal.exitCode !== undefined ? `failed (${terminal.exitCode})` : "failed",
        color: colors.red,
      };
    default:
      return { text: "unknown", color: colors.textMuted };
  }
}

function terminalLabel(terminal: Terminal): string {
  if (terminal.kind === TerminalKind.AGENT) return "Agent";
  return shellTabLabel(terminal.id, terminal.title);
}

export function TerminalsSheet({
  visible,
  terminals,
  panes = [],
  selectedTerminalId,
  selectedPageId = null,
  onSelect,
  onSelectPage,
  onCreateShell,
  onClose,
  onCloseTerminal,
}: {
  visible: boolean;
  terminals: Terminal[];
  /** The session's panes in swipe order, which the list groups apart from shell tabs. */
  panes?: readonly SwipePane[];
  selectedTerminalId: bigint;
  /** The page pane on screen, when one is. */
  selectedPageId?: string | null;
  onSelect: (terminal: Terminal) => void;
  onSelectPage?: (paneId: string) => void;
  onCreateShell: () => void;
  onClose: () => void;
  onCloseTerminal: (terminalId: bigint) => void;
}) {
  const groups = terminalGroups(terminals, panes);
  const terminalRow = (terminal: Terminal, label: string) => {
    const selected = selectedPageId === null && terminal.id === selectedTerminalId;
    const status = runStateLabel(terminal);
    const isShell = terminal.kind === TerminalKind.SHELL;
    const alive = terminal.state === TerminalRunState.RUNNING || terminal.state === TerminalRunState.STARTING;
    return (
      <View key={terminal.id.toString()} style={[styles.row, selected && styles.rowSelected]}>
        <Pressable style={styles.rowContent} onPress={() => onSelect(terminal)}>
          <View style={styles.rowLeft}>
            <Text style={[styles.terminalLabel, selected && styles.terminalLabelSelected]}>{label}</Text>
            {terminal.cwd ? (
              <Text style={styles.cwd} numberOfLines={1}>{terminal.cwd}</Text>
            ) : null}
          </View>
          <Text style={[styles.status, { color: status.color }]}>{status.text}</Text>
        </Pressable>
        {isShell && alive ? (
          <Pressable
            style={styles.closeButton}
            onPress={() => onCloseTerminal(terminal.id)}
            hitSlop={8}
          >
            <Text style={styles.closeButtonText}>Close</Text>
          </Pressable>
        ) : null}
      </View>
    );
  };
  const pageRow = (entry: Extract<PaneEntry, { kind: "page" }>) => {
    const selected = selectedPageId === entry.paneId;
    return (
      <View key={entry.paneId} style={[styles.row, selected && styles.rowSelected]}>
        <Pressable style={styles.rowContent} onPress={() => onSelectPage?.(entry.paneId)}>
          <View style={styles.rowLeft}>
            <Text style={[styles.terminalLabel, selected && styles.terminalLabelSelected]}>{entry.title}</Text>
          </View>
          <Text style={[styles.status, { color: colors.textMuted }]}>page</Text>
        </Pressable>
      </View>
    );
  };
  return (
    <Modal visible={visible} animationType="slide" presentationStyle="pageSheet" onRequestClose={onClose}>
      <SafeAreaView style={styles.root}>
        <View style={styles.header}>
          <Text style={styles.title}>Terminals</Text>
          <Pressable onPress={onClose} hitSlop={8}>
            <Text style={styles.closeText}>Done</Text>
          </Pressable>
        </View>
        <ScrollView style={styles.list} contentContainerStyle={styles.listContent}>
          {groups.agent.length > 0 ? <Text style={styles.section}>Agent</Text> : null}
          {groups.agent.map((terminal) => terminalRow(terminal, terminalLabel(terminal)))}
          {groups.panes.length > 0 ? <Text style={styles.section}>Panes</Text> : null}
          {groups.panes.map((entry) => entry.kind === "page" ? pageRow(entry) : terminalRow(entry.terminal, entry.title))}
          {groups.tabs.length > 0 ? <Text style={styles.section}>Shell tabs</Text> : null}
          {groups.tabs.map((terminal) => terminalRow(terminal, terminalLabel(terminal)))}
          <Pressable style={styles.addShell} onPress={onCreateShell}>
            <Text style={styles.addShellText}>+ New shell</Text>
          </Pressable>
        </ScrollView>
      </SafeAreaView>
    </Modal>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1, backgroundColor: colors.bg },
  header: {
    flexDirection: "row",
    justifyContent: "space-between",
    alignItems: "center",
    paddingHorizontal: 16,
    paddingVertical: 14,
    borderBottomWidth: 1,
    borderBottomColor: colors.line,
  },
  title: { color: colors.textBright, fontSize: 17, fontWeight: "600" },
  closeText: { color: colors.blue, fontSize: 15 },
  list: { flex: 1 },
  section: {
    color: colors.textMuted,
    fontSize: 11,
    fontWeight: "600",
    letterSpacing: 0.5,
    textTransform: "uppercase",
    marginTop: 6,
    marginLeft: 2,
  },
  listContent: { padding: 16, gap: 8 },
  row: {
    backgroundColor: colors.panel,
    borderRadius: 10,
    borderWidth: 1,
    borderColor: "transparent",
    overflow: "hidden",
  },
  rowSelected: {
    borderColor: colors.blue,
  },
  rowContent: {
    flexDirection: "row",
    alignItems: "center",
    justifyContent: "space-between",
    padding: 14,
    gap: 12,
  },
  rowLeft: { flex: 1 },
  terminalLabel: { color: colors.text, fontSize: 15 },
  terminalLabelSelected: { color: colors.textBright, fontWeight: "600" },
  cwd: { color: colors.textMuted, fontSize: 12, marginTop: 2, fontFamily: "Menlo" },
  status: { fontSize: 12 },
  closeButton: {
    borderTopWidth: 1,
    borderTopColor: colors.line,
    paddingVertical: 10,
    alignItems: "center",
  },
  closeButtonText: { color: colors.red, fontSize: 13, fontWeight: "500" },
  addShell: {
    backgroundColor: colors.panelAlt,
    borderRadius: 10,
    paddingVertical: 14,
    alignItems: "center",
  },
  addShellText: { color: colors.blue, fontSize: 15, fontWeight: "500" },
});
