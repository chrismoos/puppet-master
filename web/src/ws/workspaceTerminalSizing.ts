import type { TerminalSize } from "@puppet-master/client-core/ws/pty";

export type WorkspacePtyEchoDisposition = "current" | "pending" | "snapshot" | "apply";

export interface WorkspaceInputReadiness {
  initialSnapshotParsed: boolean;
  pendingWrites: number;
  pendingEchoedSize: TerminalSize | null;
  requestedSize: TerminalSize | null;
  echoedSize: TerminalSize | null;
  promptBlocked: boolean;
  allowMissingEcho: boolean;
}

function sameSize(left: TerminalSize | null, right: TerminalSize): boolean {
  return left !== null && left.cols === right.cols && left.rows === right.rows;
}

export function workspaceInputReady(state: WorkspaceInputReadiness): boolean {
  if (!state.initialSnapshotParsed || state.pendingWrites > 0 || state.pendingEchoedSize !== null) return false;
  if (state.promptBlocked) return true;
  if (state.echoedSize === null) return state.allowMissingEcho;
  return sameSize(state.requestedSize, state.echoedSize);
}

export type WorkspaceInputDisposition = "send" | "hold";

export function workspaceInputDisposition(
  inputReady: boolean,
  queuedEntries: number,
): WorkspaceInputDisposition {
  if (inputReady && queuedEntries === 0) return "send";
  return "hold";
}

export function workspaceResizeRequest(
  measured: TerminalSize,
  lastRequested: TerminalSize,
  echoed: TerminalSize | null,
): TerminalSize | null {
  if (sameSize(echoed, measured) || sameSize(lastRequested, measured)) return null;
  return measured;
}

export function workspacePtyEchoDisposition(
  current: TerminalSize,
  pending: TerminalSize | null,
  echoed: TerminalSize,
  active: boolean,
  replayPainted: boolean,
): WorkspacePtyEchoDisposition {
  if (sameSize(current, echoed)) return "current";
  if (sameSize(pending, echoed)) return "pending";
  if (active && replayPainted) return "snapshot";
  return "apply";
}
