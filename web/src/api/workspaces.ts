import type { WorkspaceLayout } from "@puppet-master/client-core/state/workspace";
import { authedFetch } from "./token";

const HTTP_UNAUTHORIZED = 401;

export interface SavedWorkspace {
  id: number;
  name: string;
  layout: WorkspaceLayout;
  createdAtUnixMs: number;
  updatedAtUnixMs: number;
  position: number;
}

export interface WorkspaceListing {
  workspaces: SavedWorkspace[];
}

async function ensureOk(response: Response): Promise<void> {
  if (response.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  if (!response.ok) {
    const body = (await response.json().catch(() => null)) as { error?: string } | null;
    throw new Error(body?.error || `workspace request failed (${response.status})`);
  }
}

export async function listWorkspaces(): Promise<WorkspaceListing> {
  const response = await authedFetch("/api/workspaces");
  await ensureOk(response);
  return (await response.json()) as WorkspaceListing;
}

export async function reorderWorkspaces(workspaceIds: number[]): Promise<void> {
  const response = await authedFetch("/api/workspaces/order", {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ workspace_ids: workspaceIds }),
  });
  await ensureOk(response);
}

export async function createWorkspace(name: string, layout: WorkspaceLayout): Promise<SavedWorkspace> {
  const response = await authedFetch("/api/workspaces", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name, layout }),
  });
  await ensureOk(response);
  return (await response.json()) as SavedWorkspace;
}

export async function updateWorkspace(workspace: SavedWorkspace): Promise<SavedWorkspace> {
  const response = await authedFetch(`/api/workspaces/${workspace.id}`, {
    method: "PUT",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ name: workspace.name, layout: workspace.layout }),
  });
  await ensureOk(response);
  return (await response.json()) as SavedWorkspace;
}

export async function deleteWorkspace(id: number): Promise<void> {
  const response = await authedFetch(`/api/workspaces/${id}`, { method: "DELETE" });
  await ensureOk(response);
}
