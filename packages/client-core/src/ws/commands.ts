import { create } from "@bufbuild/protobuf";
import {
  CreateProjectSchema,
  SetBucketPermissionModeSchema,
  SetProjectPermissionModeSchema,
  type ClientMessage,
  type CommandResult,
  type PermissionMode,
} from "../gen/pm/v1/pm_pb";

/** Seq used for PtyInput/PtyResize, which never get a CommandResult. */
export const FIRE_AND_FORGET_SEQ = 0n;

type ClientMessagePayload = ClientMessage["msg"];

export function createProjectMsg(
  bucketId: bigint,
  name: string,
  path: string,
  workerId?: bigint,
  allowedWorkerIds: bigint[] = [],
): ClientMessagePayload {
  return {
    case: "createProject",
    value: create(CreateProjectSchema, { bucketId, name, path, workerId, allowedWorkerIds }),
  };
}

export function setBucketPermissionModeMsg(
  bucketId: bigint,
  mode: PermissionMode,
): ClientMessagePayload {
  return {
    case: "setBucketPermissionMode",
    value: create(SetBucketPermissionModeSchema, { bucketId, mode }),
  };
}

export function setProjectPermissionModeMsg(
  projectId: bigint,
  mode: PermissionMode,
): ClientMessagePayload {
  return {
    case: "setProjectPermissionMode",
    value: create(SetProjectPermissionModeSchema, { projectId, mode }),
  };
}

export interface CommandOutcome {
  createdId?: bigint;
  data?: Uint8Array;
}

interface PendingCommand {
  resolve(outcome: CommandOutcome): void;
  reject(err: Error): void;
}

/** Correlates client-chosen seq numbers with server CommandResults. */
export class CommandTracker {
  private seq = FIRE_AND_FORGET_SEQ;
  private pending = new Map<bigint, PendingCommand>();

  nextSeq(): bigint {
    this.seq += 1n;
    return this.seq;
  }

  register(seq: bigint): Promise<CommandOutcome> {
    return new Promise((resolve, reject) => {
      this.pending.set(seq, { resolve, reject });
    });
  }

  /** Results for unknown seqs (stale connection, replays) are ignored. */
  settle(result: CommandResult): void {
    const entry = this.pending.get(result.seq);
    if (!entry) return;
    this.pending.delete(result.seq);
    if (result.ok) {
      entry.resolve({ createdId: result.createdId, ...(result.data.length > 0 ? { data: result.data } : {}) });
    } else {
      entry.reject(new Error(result.error || "command failed"));
    }
  }

  failAll(reason: string): void {
    const entries = [...this.pending.values()];
    this.pending.clear();
    for (const entry of entries) {
      entry.reject(new Error(reason));
    }
  }

  get pendingCount(): number {
    return this.pending.size;
  }
}
