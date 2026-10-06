import { LOCAL_WORKER_ID } from "../format";
import { ConnectMode, type Bucket, type Project, type Worker } from "../gen/pm/v1/pm_pb";

/**
 * The worker a project's sessions run on: its own override, else the
 * bucket's default, else the local worker.
 */
export function resolveProjectWorkerId(
  project: Pick<Project, "workerId"> | undefined,
  bucket: Pick<Bucket, "defaultWorkerId"> | undefined,
): bigint {
  if (project?.workerId !== undefined) return project.workerId;
  if (bucket?.defaultWorkerId !== undefined) return bucket.defaultWorkerId;
  return LOCAL_WORKER_ID;
}

/** Workers ordered with the local worker first, then by id. */
export function orderWorkers<T extends { id: bigint }>(workers: readonly T[]): T[] {
  return [...workers].sort((a, b) => (a.id < b.id ? -1 : a.id > b.id ? 1 : 0));
}

/** The fields of a Worker that describe how its connection is opened. */
export interface WorkerConnection {
  connectMode?: ConnectMode;
  endpoint?: string;
}

/** True when the controller opens the connection to this worker. */
export function isDialedHost(worker: WorkerConnection | undefined): boolean {
  return worker?.connectMode === ConnectMode.ACCEPT;
}

export interface HostConnectionView {
  /** Short badge text for the Workers row. */
  label: string;
  /** The address the controller dials, empty unless this worker is dialed. */
  endpoint: string;
  title: string;
}

/**
 * How a Workers row presents which end opens the connection. The local
 * worker has no worker plane connection at all, so it reads as neither.
 */
export function hostConnectionView(
  worker: WorkerConnection & { id: bigint },
): HostConnectionView | null {
  if (worker.id === LOCAL_WORKER_ID) return null;
  if (isDialedHost(worker)) {
    const endpoint = worker.endpoint ?? "";
    return {
      label: "controller dials",
      endpoint,
      title: endpoint
        ? `the controller opens the connection to ${endpoint}`
        : "the controller opens the connection, but no address is recorded",
    };
  }
  return {
    label: "dials controller",
    endpoint: "",
    title: "this Worker opens the connection to the controller",
  };
}

export interface HostRuntimeView {
  /** Short badge text naming the runtime and the container. */
  label: string;
  title: string;
}

/**
 * How a Hosts row presents the runtime holding a Host. One that reports
 * none runs on its machine directly, which is also what every Host
 * looked like before they reported it, so it shows nothing rather than
 * claiming the Host is bare metal.
 *
 * The title carries the log command because the controller cannot reach
 * the runtime: reading the log or restarting the container is something
 * only someone on that machine can do.
 */
export function hostRuntimeView(
  worker: Pick<Partial<Worker>, "runtime" | "container">,
): HostRuntimeView | null {
  const runtime = worker.runtime ?? "";
  if (!runtime) return null;
  const container = worker.container ?? "";
  if (!container) {
    return { label: runtime, title: `this Host runs under ${runtime}` };
  }
  return {
    label: `${runtime}:${container}`,
    title:
      `this Host runs in a ${runtime} container named ${container}. ` +
      `Read its log on that machine with: ${runtime} logs -f ${container}`,
  };
}

/**
 * Why an offline Worker is offline, stated in terms of which end opens the
 * connection: the controller cannot reach a dialed Worker, while a Worker
 * that dials the controller has simply not done so.
 */
export function offlineHostReason(worker: WorkerConnection): string {
  if (!isDialedHost(worker)) return "this Worker has not connected to the controller";
  return worker.endpoint
    ? `the controller cannot reach this Worker at ${worker.endpoint}`
    : "the controller cannot reach this Worker";
}

export function workerUnavailableReason(
  workers: ReadonlyMap<string, WorkerConnection & { online: boolean }>,
  workerId: bigint,
): string | null {
  const worker = workers.get(workerId.toString());
  if (worker?.online) return null;
  if (workerId === LOCAL_WORKER_ID && !worker) {
    return workers.size === 0
      ? "no workers registered; local worker is disabled for this daemon"
      : "local worker is disabled for this daemon";
  }
  if (!worker) return `worker ${workerId} is not registered`;
  if (workerId === LOCAL_WORKER_ID) return "local worker is offline";
  if (isDialedHost(worker)) {
    return worker.endpoint
      ? `worker ${workerId} cannot be reached at ${worker.endpoint}`
      : `worker ${workerId} cannot be reached`;
  }
  return `worker ${workerId} has not connected`;
}
