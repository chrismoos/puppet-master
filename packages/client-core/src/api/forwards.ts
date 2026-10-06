import type { JsonFetch } from "../platform";

/** Query parameter the forward proxy reads the scoped token from. */
export const FORWARD_TOKEN_QUERY_PARAM = "fwd_token";

const HTTP_UNAUTHORIZED = 401;

export interface ForwardToken {
  token: string;
  expiresInMs: number;
}

/** HTTP-level failure minting a forward token, keeping the status code. */
export class ForwardTokenError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "ForwardTokenError";
  }
}

export function forwardTokenPath(forwardId: string): string {
  return `/api/forwards/${encodeURIComponent(forwardId)}/token`;
}

/** Parse the daemon's `{ token, expiresInMs }` response. */
export function parseForwardTokenResponse(payload: unknown): ForwardToken {
  const obj = (payload ?? {}) as { token?: unknown; expiresInMs?: unknown };
  if (typeof obj.token !== "string" || !obj.token || typeof obj.expiresInMs !== "number") {
    throw new Error("malformed forward token response");
  }
  return { token: obj.token, expiresInMs: obj.expiresInMs };
}

/** Safari's initial navigation carries the scoped token instead of the app bearer. */
export function forwardUrlWithToken(url: string, token: string): string {
  const hashAt = url.indexOf("#");
  const base = hashAt === -1 ? url : url.slice(0, hashAt);
  const fragment = hashAt === -1 ? "" : url.slice(hashAt);
  const sep = base.includes("?") ? "&" : "?";
  return `${base}${sep}${FORWARD_TOKEN_QUERY_PARAM}=${encodeURIComponent(token)}${fragment}`;
}

/**
 * Mints a short-lived token scoped to one forward. The adapter supplies the
 * caller's own credential (session cookie on web, bearer token on mobile).
 */
export async function mintForwardToken(fetchImpl: JsonFetch, forwardId: string, destination?: string): Promise<ForwardToken> {
  const path = forwardTokenPath(forwardId) + (destination === undefined ? "" : `?destination=${encodeURIComponent(destination)}`);
  const res = await fetchImpl(path, { method: "POST" });
  if (!res.ok) {
    if (res.status === HTTP_UNAUTHORIZED) {
      throw new ForwardTokenError(res.status, "not authenticated");
    }
    const body = (await res.json().catch(() => null)) as { error?: unknown } | null;
    const detail = typeof body?.error === "string" ? body.error : `minting a forward token failed (${res.status})`;
    throw new ForwardTokenError(res.status, detail);
  }
  return parseForwardTokenResponse(await res.json());
}

/**
 * The URL a client should actually navigate to for a published forward:
 * the clean URL plus a freshly minted scoped token. Every client must go
 * through this so web and mobile cannot drift on how the credential rides.
 */
export async function openableForwardUrl(fetchImpl: JsonFetch, forwardId: string, url: string, validateDestination = false): Promise<string> {
  const { token } = await mintForwardToken(fetchImpl, forwardId, validateDestination ? url : undefined);
  return forwardUrlWithToken(url, token);
}
