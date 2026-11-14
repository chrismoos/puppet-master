import type { Terminal } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import type { TerminalStreamStatus } from "@puppet-master/client-core/ws/terminalSocket";
import { navigate, settingsRoutePath } from "../router";
import { useAppState } from "../state/hooks";
import { terminalStatusView, terminalStatusWorker } from "../ws/terminalStatusView";
import { TerminalStatus } from "./TerminalStatus";

/** The connection status of a terminal the desktop app draws natively, in
 * the same words and actions a browser terminal uses. */
export function NativeTerminalStatus({ status, terminal, onRetry }: {
  status: TerminalStreamStatus;
  terminal: Terminal;
  onRetry: () => void;
}) {
  const state = useAppState();
  const view = terminalStatusView(status, {
    connecting: false,
    worker: terminalStatusWorker(state, { terminalId: terminal.id }),
  });
  if (!view) return null;
  return (
    <TerminalStatus
      view={view}
      onAction={(action) => {
        if (action === "retry") onRetry();
        else if (action === "reload") location.reload();
        else navigate(settingsRoutePath("workers"));
      }}
    />
  );
}
