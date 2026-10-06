import { authedFetch } from "./token";

const HTTP_UNAUTHORIZED = 401;

/**
 * Why a project cannot run on a host. Only "host-offline" is about the
 * network; the rest are configuration on a host that is up, which is the
 * distinction the dashboard has to keep visible.
 */
export type ProjectHostStatus =
  | "ready"
  | "host-offline"
  | "path-unset"
  | "path-missing"
  | "path-not-a-directory"
  | "path-unreadable"
  | "path-unchecked";

export interface ProjectHostState {
  status: ProjectHostStatus;
  /** The resolved path the verdict is about; empty when none resolved. */
  path: string;
  /** One sentence naming the project, the host, and what to change. */
  detail: string;
}

/** Whether this verdict should stop a spawn rather than only inform it. */
export function blocksSpawn(state: ProjectHostState | null): boolean {
  return state !== null && state.status !== "ready" && state.status !== "path-unchecked";
}

/** Asks the controller how a project stands on one host. */
export async function fetchProjectHost(
  projectId: bigint,
  workerId: bigint,
  signal?: AbortSignal,
): Promise<ProjectHostState> {
  const query = new URLSearchParams({
    project: projectId.toString(),
    worker: workerId.toString(),
  });
  const res = await authedFetch(`/api/project-host?${query.toString()}`, { signal });
  if (res.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  if (!res.ok) {
    const body = (await res.json().catch(() => null)) as { error?: string } | null;
    throw new Error(body?.error || `project host check failed (${res.status})`);
  }
  return (await res.json()) as ProjectHostState;
}
