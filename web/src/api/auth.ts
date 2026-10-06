import { accessToken, authedFetch, forgetAccessToken, UnauthenticatedError } from "./token";

const HTTP_UNAUTHORIZED = 401;
const HTTP_NOT_FOUND = 404;

export type MeResult =
  | { kind: "user"; username: string }
  | { kind: "anonymous" }
  | { kind: "setup" };

/**
 * Who the browser is signed in as.
 *
 * Minting is the first thing asked, because it is the only thing the session
 * cookie still answers for and every later call needs the token anyway. A
 * refused mint means no session, and an install with no user at all answers
 * the unauthenticated probe differently from one that is merely signed out.
 */
export async function fetchMe(): Promise<MeResult> {
  try {
    await accessToken();
  } catch (err) {
    if (err instanceof UnauthenticatedError) return signedOutKind();
    throw err;
  }
  const res = await authedFetch("/api/me");
  if (res.ok) {
    const body = (await res.json()) as { username: string };
    return { kind: "user", username: body.username };
  }
  if (res.status === HTTP_UNAUTHORIZED) return signedOutKind();
  throw new Error(`unexpected response from /api/me (${res.status})`);
}

/** Whether this install has no user yet or merely no session. Asked without
 * credentials, because neither answer is one. */
async function signedOutKind(): Promise<MeResult> {
  const res = await fetch("/api/me");
  if (res.status === HTTP_NOT_FOUND) {
    const body = (await res.json().catch(() => null)) as { setup?: boolean } | null;
    if (body?.setup) return { kind: "setup" };
  }
  return { kind: "anonymous" };
}

/** The daemon refuses with `{"error": "..."}`, which is the sentence a person
 * should read. Without this the body reaches the screen as its own JSON. */
async function refusalReason(res: Response): Promise<string | null> {
  const body = (await res.json().catch(() => null)) as { error?: unknown } | null;
  if (typeof body?.error !== "string") return null;
  const reason = body.error.trim();
  return reason === "" ? null : reason;
}

async function postCredentials(path: string, username: string, password: string): Promise<void> {
  const res = await fetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ username, password }),
  });
  if (res.ok) {
    forgetAccessToken();
    return;
  }
  // A rejected credential says so in terms that name both fields, which is
  // kinder than the daemon's own wording for the same status.
  if (res.status === HTTP_UNAUTHORIZED) {
    throw new Error("invalid username or password");
  }
  const reason = await refusalReason(res);
  throw new Error(reason ?? `request failed (${res.status})`);
}

export function login(username: string, password: string): Promise<void> {
  return postCredentials("/api/login", username, password);
}

export function setup(username: string, password: string): Promise<void> {
  return postCredentials("/api/setup", username, password);
}

/** The daemon answers a rejected change with its own reason, which
 * distinguishes a wrong current password from one that is too short. */
export async function changePassword(
  currentPassword: string,
  newPassword: string,
): Promise<void> {
  const res = await fetch("/api/user/password", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ currentPassword, newPassword }),
  });
  if (res.ok) return;
  const reason = await refusalReason(res);
  if (reason) throw new Error(reason);
  if (res.status === HTTP_UNAUTHORIZED) {
    throw new Error("invalid credentials");
  }
  throw new Error(`request failed (${res.status})`);
}

export async function logout(): Promise<void> {
  await fetch("/api/logout", { method: "POST" });
  forgetAccessToken();
}

export interface BuildInfo {
  version: string;
  gitRev: string;
  /** The release channel `pm update` on the controller host follows. */
  channel: string;
  /** Port hosts connect to, or null when this controller serves none. */
  hostPlanePort: number | null;
  /** The base URL the operator configured the daemon with, or null. */
  publicUrl: string | null;
  /** How this build installs pm, for a machine that has none yet. */
  installCommand: string;
  /** The controller's own OS, which a Worker on this machine shares. */
  platform: string;
  /**
   * Where published previews are mounted. "path-prefix" puts them on the
   * dashboard's own origin, which the forwards bar says out loud because it
   * means a preview holds this user's authority.
   */
  forwardMount: "path-prefix" | "share-domain" | "per-forward-port" | null;
}

export async function fetchVersion(): Promise<BuildInfo | null> {
  try {
    const res = await fetch("/api/version");
    if (!res.ok) return null;
    const body = (await res.json()) as Partial<BuildInfo> & {
      forwardMount?: { mode?: string };
    };
    return {
      version: body.version ?? "",
      gitRev: body.gitRev ?? "",
      channel: body.channel ?? "stable",
      hostPlanePort: body.hostPlanePort ?? null,
      publicUrl: body.publicUrl ?? null,
      installCommand: body.installCommand ?? "",
      platform: body.platform ?? "",
      forwardMount: (body.forwardMount?.mode as BuildInfo["forwardMount"]) ?? null,
    };
  } catch {
    return null;
  }
}
