// A WebSocket handshake carries no Authorization header a page can set, so the
// dashboard spends its access token on a one-use ticket and puts that in the
// URL instead. The ticket is the mechanism the phone's terminal WebView
// already uses for the same reason, and it is worth far less than the token:
// it opens one socket, once, within seconds of being minted.

import { authedFetch } from "../api/token";

interface TicketRequest {
  path: string;
  body?: string;
}

interface MintedTicket {
  ticket: string;
}

/**
 * Which endpoint mints the ticket for a socket path, and what it needs told.
 *
 * A terminal ticket is bound to the terminal and the generation it was minted
 * against, so the generation the socket is about to ask for is the one the
 * mint has to name.
 */
export function ticketRequestFor(socketPath: string): TicketRequest | null {
  const [path, query] = socketPath.split("?", 2);
  if (path === "/ws") {
    return { path: "/api/ws/ticket" };
  }
  const terminal = /^\/ws\/terminal\/(\d+)$/.exec(path);
  if (!terminal) return null;
  // Absent reads as 0 through Number, which would mint a ticket bound to a
  // generation the socket never asked for.
  const stated = new URLSearchParams(query ?? "").get("generation");
  if (stated === null || stated === "") return null;
  const generation = Number(stated);
  if (!Number.isSafeInteger(generation) || generation < 0) return null;
  return {
    path: `/api/terminals/${terminal[1]}/attach-ticket`,
    body: JSON.stringify({ generation }),
  };
}

/** Appends a ticket to a socket path that may already carry a query. */
export function withTicket(socketPath: string, ticket: string): string {
  const separator = socketPath.includes("?") ? "&" : "?";
  return `${socketPath}${separator}ticket=${encodeURIComponent(ticket)}`;
}

/** Mints the one-use ticket a socket path needs, or null when the path needs
 * none. */
export async function mintSocketTicket(socketPath: string): Promise<string | null> {
  const request = ticketRequestFor(socketPath);
  if (!request) return null;
  const res = await authedFetch(request.path, {
    method: "POST",
    ...(request.body === undefined
      ? {}
      : { headers: { "Content-Type": "application/json" }, body: request.body }),
  });
  if (!res.ok) {
    throw new Error(`could not mint a socket ticket (${res.status})`);
  }
  const body = (await res.json()) as Partial<MintedTicket>;
  if (typeof body.ticket !== "string" || body.ticket === "") {
    throw new Error("the controller minted no socket ticket");
  }
  return body.ticket;
}
