import { agentValue } from "@puppet-master/client-core/state/agent";
import type { SpawnRequest } from "../state/spawnSubmit";
import { authedFetch } from "./token";

export interface HarnessStatus {
  state: "missing" | "installing" | "ready" | "failed" | "unsupported";
  command: string;
  output: string;
  error: string;
}

export interface HarnessReply {
  agent: string;
  status: HarnessStatus;
}

export async function harnessStatus(request: SpawnRequest, install: boolean, signal: AbortSignal): Promise<HarnessReply> {
  const response = await authedFetch("/api/harness", {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({
      project: Number(request.projectId),
      worker: Number(request.workerId),
      agent: request.agent === undefined ? undefined : agentValue(request.agent),
      install,
    }),
    signal,
  });
  const body = await response.json();
  if (!response.ok) throw new Error(body.error || `Harness check failed (${response.status})`);
  return body as HarnessReply;
}
