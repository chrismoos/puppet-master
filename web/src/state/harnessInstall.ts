import { agentFromValue } from "@puppet-master/client-core/state/agent";
import { harnessStatus, type HarnessReply } from "../api/harness";
import type { SpawnRequest } from "./spawnSubmit";

const POLL_INTERVAL_MS = 750;

export class InstallCancelled extends Error {}

export async function prepareHarness(
  request: SpawnRequest,
  signal: AbortSignal,
  update: (reply: HarnessReply) => void,
  confirm: (reply: HarnessReply) => Promise<boolean>,
): Promise<SpawnRequest> {
  let reply = await harnessStatus(request, false, signal);
  const pinned = { ...request, agent: agentFromValue(reply.agent) };
  update(reply);
  if (reply.status.state === "missing" || reply.status.state === "failed") {
    if (!await confirm(reply)) throw new InstallCancelled();
    signal.throwIfAborted();
    reply = await harnessStatus(pinned, true, signal);
    update(reply);
  }
  while (reply.status.state === "installing") {
    await new Promise<void>((resolve, reject) => {
      const abort = () => { clearTimeout(timer); reject(signal.reason); };
      const timer = setTimeout(() => { signal.removeEventListener("abort", abort); resolve(); }, POLL_INTERVAL_MS);
      signal.addEventListener("abort", abort, { once: true });
      if (signal.aborted) abort();
    });
    reply = await harnessStatus(pinned, false, signal);
    update(reply);
  }
  if (reply.status.state === "failed") {
    throw new Error(reply.status.error || "Installation failed.");
  }
  if (reply.status.state !== "ready" && reply.status.state !== "unsupported") {
    throw new Error("The harness is still missing after installation.");
  }
  return pinned;
}
