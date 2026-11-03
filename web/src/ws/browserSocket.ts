import {
  TERMINAL_CLOSE_INCOMPATIBLE,
  TERMINAL_CLOSE_PAGE_OUTDATED,
} from "@puppet-master/client-core/ws/terminalFrame";
import ghosttyRevision from "./ghostty/vendor/VERSION?raw";
import { WS_CLOSE_UNAUTHENTICATED } from "@puppet-master/client-core/ws/client";
import type { SocketConnector, SocketLike } from "@puppet-master/client-core/platform";
import { authedFetch, UnauthenticatedError } from "../api/token";
import { deferredSocket, WS_CLOSE_LOCAL_FAILURE } from "./deferredSocket";
import { mintSocketTicket, withTicket } from "./socketTicket";

/**
 * Daemon WebSocket adapter: same-origin sockets authenticated by a one-use
 * ticket the dashboard mints with its access token.
 *
 * The session cookie still reaches the handshake, because the browser attaches
 * it to a same-site request whatever this code does, but the daemon no longer
 * reads it here. That is the whole point: a page sharing this site's cookie
 * jar cannot open an agent's terminal with a cookie it did not have to be able
 * to read.
 */
class TerminalCompatibilityError extends Error {
  constructor(message: string, readonly pageOutdated: boolean) {
    super(message);
  }
}

/** Set by the controller on a refusal that only reloading the page resolves. */
const INCOMPATIBLE_HEADER = "x-pm-terminal-incompatible";

export const browserSocketConnector: SocketConnector = (path, subprotocol) =>
  deferredSocket(
    async () => {
      const terminalPath = /^\/ws\/terminal\/\d+\?/.test(path)
        ? `${path}&ghostty=${encodeURIComponent(`ghostty-vt:${ghosttyRevision.trim()}:pm-v1`)}`
        : path;
      const terminal = /^\/ws\/terminal\/(\d+)\?/.exec(terminalPath);
      if (terminal) {
        const compatibility = new URLSearchParams(terminalPath.split("?", 2)[1]).get("ghostty")!;
        const response = await authedFetch(`/api/terminals/${terminal[1]}/snapshot-compatibility?ghostty=${encodeURIComponent(compatibility)}`);
        if (!response.ok) {
          const message = await response.text();
          if (response.status === 409) {
            throw new TerminalCompatibilityError(message, response.headers.get(INCOMPATIBLE_HEADER) === "page");
          }
          throw new Error(message);
        }
      }
      const ticket = await mintSocketTicket(terminalPath);
      const scheme = location.protocol === "https:" ? "wss://" : "ws://";
      const url = `${scheme}${location.host}${ticket ? withTicket(terminalPath, ticket) : terminalPath}`;
      return new WebSocket(url, subprotocol) as unknown as SocketLike;
    },
    // A ticket the signed-out cannot mint has to read as the daemon refusing
    // the socket, or the client reconnects against a session that is gone
    // instead of asking the user to sign in.
    (err) =>
      err instanceof UnauthenticatedError ? WS_CLOSE_UNAUTHENTICATED
        : err instanceof TerminalCompatibilityError
          ? err.pageOutdated ? TERMINAL_CLOSE_PAGE_OUTDATED : TERMINAL_CLOSE_INCOMPATIBLE
          : WS_CLOSE_LOCAL_FAILURE,
  );
