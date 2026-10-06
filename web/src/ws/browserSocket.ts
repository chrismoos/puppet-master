import { WS_CLOSE_UNAUTHENTICATED } from "@puppet-master/client-core/ws/client";
import type { SocketConnector, SocketLike } from "@puppet-master/client-core/platform";
import { UnauthenticatedError } from "../api/token";
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
export const browserSocketConnector: SocketConnector = (path, subprotocol) =>
  deferredSocket(
    async () => {
      const ticket = await mintSocketTicket(path);
      const scheme = location.protocol === "https:" ? "wss://" : "ws://";
      const url = `${scheme}${location.host}${ticket ? withTicket(path, ticket) : path}`;
      return new WebSocket(url, subprotocol) as unknown as SocketLike;
    },
    // A ticket the signed-out cannot mint has to read as the daemon refusing
    // the socket, or the client reconnects against a session that is gone
    // instead of asking the user to sign in.
    (err) =>
      err instanceof UnauthenticatedError ? WS_CLOSE_UNAUTHENTICATED : WS_CLOSE_LOCAL_FAILURE,
  );
