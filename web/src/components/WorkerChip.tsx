import type { Worker } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { LOCAL_WORKER_ID } from "@puppet-master/client-core/format";

export function WorkerChip({ worker }: { worker: Worker }) {
  const title = worker.hostname ? `${worker.name} (${worker.hostname})` : worker.name;
  return (
    <span
      className={`worker-chip ${worker.online ? "" : "worker-chip-offline"}`}
      title={title}
    >
      <span className="worker-chip-dot" aria-hidden="true" />
      <span className="worker-chip-name">{worker.name}</span>
      {!worker.online && <span className="worker-chip-state">offline</span>}
    </span>
  );
}

export function UnavailableWorkerChip({ workerId }: { workerId: bigint }) {
  const local = workerId === LOCAL_WORKER_ID;
  return (
    <span className="worker-chip worker-chip-offline" title={local ? "Local worker is disabled for this daemon" : `Worker ${workerId} is not registered`}>
      <span className="worker-chip-dot" aria-hidden="true" />
      <span className="worker-chip-name">{local ? "local" : `worker ${workerId}`}</span>
      <span className="worker-chip-state">{local ? "disabled" : "unavailable"}</span>
    </span>
  );
}
