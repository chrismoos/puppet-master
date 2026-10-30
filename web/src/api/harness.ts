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
  const text = await response.text();
  let body: HarnessReply & { error?: string };
  try {
    body = JSON.parse(text);
  } catch {
    if (!response.ok) throw new Error(text.trim() || `Harness check failed (${response.status})`);
    throw new Error("The harness check returned an invalid response. Try again.");
  }
  if (!response.ok) throw new Error(body?.error || `Harness check failed (${response.status})`);
  if (!body?.agent || !body.status?.state) {
    throw new Error("The harness check returned an invalid response. Try again.");
  }
  return body;
}
