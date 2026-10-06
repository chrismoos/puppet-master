import { SessionRole, type Project } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { readStringMap, writeStringMap } from "../storage";

export const SUPERVISOR_SPAWN_PROJECTS_KEY = "pm.supervisorSpawnProjects";
export const WORKER_SPAWN_PROJECTS_KEY = "pm.workerSpawnProjects";

function keyForRole(role: SessionRole): string {
  return role === SessionRole.SUPERVISOR ? SUPERVISOR_SPAWN_PROJECTS_KEY : WORKER_SPAWN_PROJECTS_KEY;
}

/// The bucket popover defaults a project rather than leading with it: the one
/// last used for this role in this bucket, else the bucket's first by name. A
/// supervisor's project is only its runtime home — cwd, host, permission
/// cascade — never its scope of authority.
export function defaultSpawnProjectId(
  bucketId: string,
  projects: Iterable<Project>,
  remembered: string | undefined,
): string {
  const inBucket = [...projects]
    .filter((project) => project.bucketId.toString() === bucketId)
    .sort((a, b) => a.name.localeCompare(b.name));
  if (remembered !== undefined && inBucket.some((p) => p.id.toString() === remembered)) {
    return remembered;
  }
  return inBucket[0]?.id.toString() ?? "";
}

export function readRememberedSpawnProject(
  bucketId: string,
  role: SessionRole,
): string | undefined {
  return readStringMap(keyForRole(role)).get(bucketId);
}

export function rememberSpawnProject(
  bucketId: string,
  role: SessionRole,
  projectId: string,
): void {
  const key = keyForRole(role);
  const map = readStringMap(key);
  map.set(bucketId, projectId);
  writeStringMap(key, map);
}
