import { Modal, Pressable, SafeAreaView, ScrollView, StyleSheet, Text, View } from "react-native";

import type { Terminal } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { TerminalKind, TerminalRunState } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { shellTabLabel } from "@puppet-master/client-core/terminal/label";

import { colors } from "../theme";

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
  selectedTerminalId,
  onSelect,
  onCreateShell,
  onClose,
  onCloseTerminal,
}: {
  visible: boolean;
  terminals: Terminal[];
  selectedTerminalId: bigint;
  onSelect: (terminal: Terminal) => void;
  onCreateShell: () => void;
  onClose: () => void;
  onCloseTerminal: (terminalId: bigint) => void;
}) {
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
          {terminals.map((terminal) => {
            const selected = terminal.id === selectedTerminalId;
            const status = runStateLabel(terminal);
            const isShell = terminal.kind === TerminalKind.SHELL;
            const alive = terminal.state === TerminalRunState.RUNNING || terminal.state === TerminalRunState.STARTING;
            return (
              <View key={terminal.id.toString()} style={[styles.row, selected && styles.rowSelected]}>
                <Pressable style={styles.rowContent} onPress={() => onSelect(terminal)}>
                  <View style={styles.rowLeft}>
                    <Text style={[styles.terminalLabel, selected && styles.terminalLabelSelected]}>
                      {terminalLabel(terminal)}
                    </Text>
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
          })}
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
