import { LOCAL_WORKER_ID } from "../format";

export interface SpawnCwdState {
  value: string;
  touched: boolean;
}

export type SpawnCwdAction =
  | { type: "edit"; value: string }
  | { type: "default-changed"; value: string }
  | { type: "selection-changed"; value: string };

export function defaultSpawnCwd(
  project: { path: string; workerPaths: { workerId: bigint; path: string }[] } | undefined,
  workerId: bigint,
): string {
  if (workerId === LOCAL_WORKER_ID) return project?.path ?? "";
  return project?.workerPaths.find((mapping) => mapping.workerId === workerId)?.path
    ?? project?.path
    ?? "";
}

export function initializeSpawnCwd(value: string): SpawnCwdState {
  return { value, touched: false };
}

export function spawnCwdReducer(state: SpawnCwdState, action: SpawnCwdAction): SpawnCwdState {
  switch (action.type) {
    case "edit":
      return { value: action.value, touched: true };
    case "selection-changed":
      return initializeSpawnCwd(action.value);
    case "default-changed":
      return state.touched ? state : initializeSpawnCwd(action.value);
  }
}
