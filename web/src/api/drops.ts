import { authedFetch } from "./token";

/** Matches the daemon's own limit, so an oversized file is refused here
 * rather than after the bytes have crossed the network. */
export const SESSION_DROP_FILE_MAX = 10 * 1024 * 1024;

export interface SessionDropResult {
  path: string;
  /** False when the file was stored but its path did not reach the
   * composer, which `detail` then explains. */
  typed: boolean;
  detail: string;
}

/** Stores one dropped file on the host running `sessionId` and types its
 * path into `terminalId`, or into the session's agent terminal when no
 * terminal is named. */
export async function uploadSessionDrop(
  sessionId: string,
  terminalId: string | null,
  file: File,
  signal?: AbortSignal,
): Promise<SessionDropResult> {
  const query = new URLSearchParams({ filename: file.name });
  if (terminalId) query.set("terminal", terminalId);
  const res = await authedFetch(`/api/sessions/${sessionId}/drops?${query.toString()}`, {
    method: "POST",
    body: file,
    signal,
  });
  const body = (await res.json().catch(() => null)) as
    | (Partial<SessionDropResult> & { error?: string })
    | null;
  if (!res.ok) {
    throw new Error(body?.error || `the file could not be stored (${res.status})`);
  }
  return {
    path: body?.path ?? "",
    typed: body?.typed ?? false,
    detail: body?.detail ?? "",
  };
}
