import { type SocketCloseEvent, type SocketLike, type SocketMessageEvent } from "@puppet-master/client-core/platform";

/** WHATWG WebSocket ready states. */
const CONNECTING = 0;
const CLOSED = 3;

/** Close code reported when the socket could never be opened and the caller
 * named no better one. Inside the application-private range, and distinct from
 * the codes the daemon sends. */
export const WS_CLOSE_LOCAL_FAILURE = 4000;

/**
 * A socket whose real connection is opened later.
 *
 * The shared client logic asks for a socket and assigns its handlers on the
 * next line, so a connector that has to mint a ticket first cannot simply
 * return a promise. This stands in until the real socket exists, then forwards
 * every event to whichever handlers were assigned in the meantime. Nothing is
 * delivered before the real socket is open, so a caller that only sends after
 * `onopen` never races it.
 */
export function deferredSocket(
  open: () => Promise<SocketLike>,
  closeCodeFor: (err: unknown) => number = () => WS_CLOSE_LOCAL_FAILURE,
): SocketLike {
  let real: SocketLike | null = null;
  let closedBeforeOpen = false;
  let binaryType = "blob";

  const facade: SocketLike = {
    get binaryType() {
      return real ? real.binaryType : binaryType;
    },
    set binaryType(value: string) {
      binaryType = value;
      if (real) real.binaryType = value;
    },
    get readyState() {
      if (real) return real.readyState;
      return closedBeforeOpen ? CLOSED : CONNECTING;
    },
    send(data: ArrayBufferLike | Uint8Array) {
      real?.send(data);
    },
    close() {
      closedBeforeOpen = true;
      real?.close();
    },
    onopen: null as (() => void) | null,
    onmessage: null as ((event: SocketMessageEvent) => void) | null,
    onerror: null as (() => void) | null,
    onclose: null as ((event: SocketCloseEvent) => void) | null,
  };

  const failLocally = (code: number, reason: string) => {
    facade.onerror?.();
    facade.onclose?.({ code, reason });
  };

  open().then(
    (socket) => {
      real = socket;
      socket.binaryType = binaryType;
      if (closedBeforeOpen) {
        socket.close();
        return;
      }
      socket.onopen = () => facade.onopen?.();
      socket.onmessage = (event) => facade.onmessage?.(event);
      socket.onerror = () => facade.onerror?.();
      socket.onclose = (event) => facade.onclose?.(event);
    },
    (err: unknown) => {
      closedBeforeOpen = true;
      failLocally(
        closeCodeFor(err),
        err instanceof Error ? err.message : "could not open the socket",
      );
    },
  );

  return facade;
}
