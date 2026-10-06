// The dashboard's credential for everything except being served the shell and
// minting this token. The session cookie is what mints it, and the cookie can
// do nothing else, so a page that shares this site's cookie jar holds no
// authority over the controller.
//
// The token lives in this module's scope and nowhere else. localStorage,
// sessionStorage and a cookie are all readable by something other than this
// code, and a reload costs one round trip to mint again, so there is nothing
// to gain by persisting it.

const MINT_PATH = "/api/web/token";

const HTTP_UNAUTHORIZED = 401;

/** Mint again this far before expiry, so a request in flight when the clock
 * runs out is not retried for the sake of a second. */
const EXPIRY_MARGIN_MS = 30_000;

interface MintedToken {
  accessToken: string;
  expiresAtUnixMs: number;
}

let token: string | null = null;
let expiresAtUnixMs = 0;
/** Shared so a burst of calls on a cold start mints once rather than once
 * each, which would leave every caller but one holding a superseded token. */
let inFlight: Promise<string> | null = null;

export class UnauthenticatedError extends Error {
  constructor() {
    super("the dashboard session is no longer signed in");
    this.name = "UnauthenticatedError";
  }
}

function isUsable(): boolean {
  return token !== null && Date.now() + EXPIRY_MARGIN_MS < expiresAtUnixMs;
}

async function mint(): Promise<string> {
  // No Authorization header: this is the one request the cookie answers for,
  // and sending a dead token with it would only confuse the refusal.
  const res = await fetch(MINT_PATH, { method: "POST" });
  if (res.status === HTTP_UNAUTHORIZED) {
    throw new UnauthenticatedError();
  }
  if (!res.ok) {
    throw new Error(`could not mint an access token (${res.status})`);
  }
  const body = (await res.json()) as Partial<MintedToken>;
  if (typeof body.accessToken !== "string" || body.accessToken === "") {
    throw new Error("the controller minted no access token");
  }
  token = body.accessToken;
  expiresAtUnixMs = typeof body.expiresAtUnixMs === "number" ? body.expiresAtUnixMs : 0;
  return token;
}

/** The current token, minting one when there is none or it is about to
 * expire. */
export function accessToken(): Promise<string> {
  if (isUsable()) return Promise.resolve(token as string);
  if (!inFlight) {
    inFlight = mint().finally(() => {
      inFlight = null;
    });
  }
  return inFlight;
}

/** Drops the held token so the next call mints. Called when the controller
 * refuses one, which it does after a restart dropped it, and on sign-out and
 * sign-in, where the session the token belonged to is gone either way. */
export function forgetAccessToken(): void {
  token = null;
  expiresAtUnixMs = 0;
}

function withAuthorization(init: RequestInit | undefined, bearer: string): RequestInit {
  const headers = new Headers(init?.headers);
  headers.set("Authorization", `Bearer ${bearer}`);
  return { ...init, headers };
}

/**
 * Issues an authenticated request to the daemon.
 *
 * A refusal is retried once with a freshly minted token, because the
 * controller dropping its tokens on restart is ordinary rather than a sign
 * the user is signed out. A second refusal is the answer.
 */
export async function authedFetch(path: string, init?: RequestInit): Promise<Response> {
  const res = await fetch(path, withAuthorization(init, await accessToken()));
  if (res.status !== HTTP_UNAUTHORIZED) return res;
  forgetAccessToken();
  return fetch(path, withAuthorization(init, await accessToken()));
}

/** JSON body of an authenticated request, for the callers that only want the
 * parsed body and treat every failure the same way. */
export async function authedJson<T>(path: string, init?: RequestInit): Promise<T> {
  const res = await authedFetch(path, init);
  if (!res.ok) {
    throw new Error(`request failed (${res.status})`);
  }
  return (await res.json()) as T;
}
