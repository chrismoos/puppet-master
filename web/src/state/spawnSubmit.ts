import type { AgentKind, PermissionMode } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { ItemStatus, SessionRole } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import type { PmClient } from "@puppet-master/client-core/ws/client";
import { currentTerminalGeometry } from "../ws/terminal";
import { rememberSpawnProject } from "./supervisorSpawn";

/** One spawn, as both the dialog and the ＋ popover submit it. */
export interface SpawnRequest {
  projectId: string;
  agent: AgentKind | undefined;
  title: string;
  prompt: string;
  /** Explicit directory override; empty means the effective project default. */
  cwdOverride: string;
  permissionMode: PermissionMode;
  workerId: bigint;
  itemsApi: boolean;
  role: SessionRole;
  modelProfileId: bigint | undefined;
  /** When set, a successful spawn remembers this bucket's project for the role. */
  spawnBucketId?: string;
  /** Spawn-from-item: mark the item in-progress and link the created session. */
  item?: { id: bigint; bucketId: bigint };
  initialCols?: number;
  initialRows?: number;
}

export async function submitSpawn(client: PmClient, request: SpawnRequest): Promise<bigint | undefined> {
  const geometry =
    request.initialCols && request.initialRows
      ? { cols: request.initialCols, rows: request.initialRows }
      : currentTerminalGeometry();
  const outcome = await client.spawnSession(
    BigInt(request.projectId),
    request.agent,
    request.title.trim(),
    request.prompt,
    request.cwdOverride,
    request.permissionMode,
    request.workerId,
    request.itemsApi,
    request.role === SessionRole.SUPERVISOR,
    request.role,
    request.modelProfileId,
    geometry?.cols,
    geometry?.rows,
  );
  if (request.spawnBucketId !== undefined) {
    rememberSpawnProject(request.spawnBucketId, request.role, request.projectId);
  }
  // Best-effort; the spawn already succeeded.
  if (request.item !== undefined && outcome.createdId !== undefined) {
    await client
      .upsertItem({
        bucketId: request.item.bucketId,
        id: request.item.id,
        status: ItemStatus.IN_PROGRESS,
        linkSessionId: outcome.createdId,
      })
      .catch(() => undefined);
  }
  return outcome.createdId;
}
