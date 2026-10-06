import { watchForeground, type AppStateSource } from "../adapters/lifecycle";
import type { HostMessage } from "./protocol";

/**
 * Tells the terminal page when the app leaves and returns to the
 * foreground. The page treats the return as a visibility edge and
 * re-asserts its size to the PTY; while backgrounded it stays silent even
 * if its socket reconnects.
 */
export function bindTerminalVisibility(
  source: AppStateSource,
  send: (msg: HostMessage) => void,
): () => void {
  return watchForeground(source, {
    onForeground: () => send({ type: "setVisible", visible: true }),
    onBackground: () => send({ type: "setVisible", visible: false }),
  });
}
