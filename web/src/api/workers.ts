import { authedFetch } from "./token";

const HTTP_UNAUTHORIZED = 401;
const MIN_PORT = 1;
const MAX_PORT = 65535;

export interface EnrollResult {
  token: string;
  expiresAtUnixMs: number;
}

/** What holds a worker: the machine itself, a container runtime, or a VM. */
export type WorkerType = "machine" | "lima" | "docker" | "podman" | "incus";

/** The container runtimes a worker can be launched into. */
export type SandboxRuntime = "docker" | "podman" | "incus";

/**
 * Whether a worker shares the controller's machine. A local guest reaches
 * the controller through its runtime's name for the host, while a remote
 * worker needs an address that resolves from another machine.
 */
export type WorkerLocation = "local" | "remote";

export const WORKER_LOCATIONS: Record<WorkerLocation, { label: string; detail: string }> = {
  local: { label: "Local", detail: "same machine as the controller" },
  remote: { label: "Remote", detail: "another machine" },
};

export const WORKER_LOCATION_ORDER: readonly WorkerLocation[] = ["local", "remote"];

/// The platforms a Worker machine can be, as the dashboard asks about
/// them. The controller reports its own, which seeds the choice.
export type Platform = "macos" | "linux";

export const PLATFORMS: Record<Platform, string> = {
  macos: "macOS",
  linux: "Linux",
};

/** The controller's reported OS as one of the platforms we ask about. */
export function platformOf(os: string): Platform {
  return os === "macos" ? "macos" : "linux";
}

/**
 * What each holder implies for the command: the name a guest on the
 * controller's machine reaches it by, and whether a container is launched.
 */
export const WORKER_TYPES: Record<
  WorkerType,
  {
    label: string;
    guestHost?: string;
    runtime?: SandboxRuntime;
    /** Omitted means every platform. */
    platforms?: Platform[];
    /** Omitted means every location. */
    locations?: WorkerLocation[];
    /** How that machine installs what this type needs, per platform. */
    install?: Partial<Record<Platform, string>>;
  }
> = {
  machine: { label: "directly on the machine" },
  // A Lima VM is a guest of the controller's own machine, so it is never
  // offered for a worker somewhere else.
  lima: {
    label: "a Lima VM",
    guestHost: "host.lima.internal",
    locations: ["local"],
    install: { macos: "brew install lima", linux: "sudo apt install lima" },
  },
  docker: {
    label: "a Docker container",
    guestHost: "host.docker.internal",
    runtime: "docker",
    install: {
      macos: "brew install --cask docker && open -a Docker",
      linux: "sudo apt install docker.io",
    },
  },
  podman: {
    label: "a Podman container",
    guestHost: "host.containers.internal",
    runtime: "podman",
    install: {
      macos: "brew install podman && podman machine init && podman machine start",
      linux: "sudo apt install podman",
    },
  },
  // Incus has no macOS host support, so the launcher never tries it
  // there and the dashboard does not offer it.
  incus: {
    label: "an Incus container",
    runtime: "incus",
    platforms: ["linux"],
    install: { linux: "sudo apt install incus" },
  },
};

/**
 * The Worker type a host's own report implies. A host that registered
 * from a container said which runtime holds it, so re-enrolling it does
 * not have to ask again or guess.
 */
export function typeFromRuntime(runtime: string): WorkerType {
  return runtime === "docker" || runtime === "podman" || runtime === "incus"
    ? runtime
    : "machine";
}

/** Whether a Worker type can run on a platform at all. */
export function runsOn(type: WorkerType, platform: Platform): boolean {
  return WORKER_TYPES[type].platforms?.includes(platform) ?? true;
}

/** Whether a Worker type is offered for a worker in that location. */
export function offeredAt(type: WorkerType, location: WorkerLocation): boolean {
  return WORKER_TYPES[type].locations?.includes(location) ?? true;
}

/**
 * The type a command is built for. A type its platform cannot run, or one
 * its location does not offer, falls back to the machine itself so the
 * command is always one that machine can run.
 */
export function resolveWorkerType(
  type: WorkerType,
  platform: Platform,
  location: WorkerLocation,
): WorkerType {
  return runsOn(type, platform) && offeredAt(type, location) ? type : "machine";
}

/**
 * What a machine needs installed before the enrollment command works:
 * pm itself, and the runtime when the type calls for one.
 */
export function installSteps(
  type: WorkerType,
  platform: Platform,
  pmInstall: string,
): string[] {
  const steps = pmInstall ? [pmInstall] : [];
  const runtime = WORKER_TYPES[type].install?.[platform];
  if (runtime) steps.push(runtime);
  return steps;
}

export const WORKER_TYPE_ORDER: WorkerType[] = [
  "machine",
  "docker",
  "podman",
  "incus",
  "lima",
];

/**
 * Where a host dials this controller. Hosts connect to the host plane,
 * which is a separate listener from the web UI and speaks only `wss`, so
 * the browser's own origin is never the right answer on its own — the
 * controller reports the port and the operator states the name. A guest
 * on the controller's machine passes its runtime's name for the host.
 */
export function controllerOrigin(
  origin: string,
  hostPlanePort: number | null,
  guestHost?: string,
): string {
  const controller = new URL(origin);
  if (guestHost) controller.hostname = guestHost;
  controller.protocol = "wss:";
  controller.port = hostPlanePort === null ? "" : String(hostPlanePort);
  return controller.origin;
}

/**
 * The address a worker's command is built from: the public URL the daemon
 * was configured with, which is the name remote machines reach it by, or
 * the browser's own origin when none was.
 */
export function controllerBase(
  build: { publicUrl: string | null } | null,
  browserOrigin: string,
): string {
  const configured = build?.publicUrl?.trim();
  if (!configured) return browserOrigin;
  try {
    return new URL(configured).origin;
  } catch {
    return browserOrigin;
  }
}

const CONTROLLER_SCHEMES = ["http:", "https:", "ws:", "wss:"];

/**
 * Returns the address to put in the command, or null when the input is not
 * an absolute URL. The host plane speaks only `wss`, so a URL written with
 * any other scheme is corrected rather than passed through — the command
 * has to be one that works when pasted.
 */
export function normalizeControllerUrl(input: string): string | null {
  const trimmed = input.trim().replace(/\/+$/, "");
  let url: URL;
  try {
    url = new URL(trimmed);
  } catch {
    return null;
  }
  if (!CONTROLLER_SCHEMES.includes(url.protocol)) return null;
  url.protocol = "wss:";
  return url.origin;
}

/** Copy-paste command that enrolls a worker against this controller. */
export function enrollCommand(
  origin: string,
  token: string,
  workerType: WorkerType = "machine",
  hostPlanePort: number | null = null,
): string {
  return controllerCommand(
    controllerOrigin(origin, hostPlanePort, WORKER_TYPES[workerType].guestHost),
    token,
  );
}

/** Formats a command against an address that is already final. */
export function controllerCommand(
  controller: string,
  token: string,
  options: CommandOptions = {},
): string {
  return `pm worker${commandPrefix(options)} --controller ${controller} --token ${token}`;
}

/** What a generated command carries besides the connection itself. */
export interface CommandOptions {
  /** The worker's name on its own machine, which also names its container. */
  name?: string;
  /** Set when the command should launch a container rather than run here. */
  runtime?: SandboxRuntime;
}

/**
 * The flags that come before the connection. A named worker is addressed
 * by that name in every later `pm worker` command, so the command that
 * creates it carries the name the dashboard knows it by.
 */
function commandPrefix({ name, runtime }: CommandOptions): string {
  const parts: string[] = [];
  if (runtime) {
    parts.push("--sandbox", "--runtime", runtime);
  }
  const trimmed = name?.trim();
  if (trimmed) parts.push("--name", shellArg(trimmed));
  return parts.length === 0 ? "" : ` ${parts.join(" ")}`;
}

/** A worker name is operator-supplied, so it is quoted when it needs it. */
function shellArg(value: string): string {
  return /^[A-Za-z0-9._-]+$/.test(value) ? value : `'${value.replace(/'/g, "'\\''")}'`;
}

/**
 * Copy-paste command for a host the controller dials. It waits on the
 * address the controller was given rather than dialing out itself.
 */
export function listenCommand(
  endpoint: string,
  token: string,
  options: CommandOptions = {},
): string {
  return `pm worker${commandPrefix(options)} --listen ${endpoint} --token ${token}`;
}

/**
 * Returns the cleaned-up `host:port` the controller will dial, or null
 * when the input is not one. A URL is rejected because the controller
 * dials the worker plane directly rather than through a scheme.
 */
export function normalizeEndpoint(input: string): string | null {
  const trimmed = input.trim();
  if (!trimmed || trimmed.includes("/") || trimmed.includes(" ")) return null;
  const separator = trimmed.lastIndexOf(":");
  if (separator <= 0) return null;
  const host = trimmed.slice(0, separator);
  const port = Number(trimmed.slice(separator + 1));
  if (!host || !Number.isInteger(port) || port < MIN_PORT || port > MAX_PORT) return null;
  return trimmed;
}

async function ensureOk(res: Response, what: string): Promise<void> {
  if (res.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  if (!res.ok) {
    const body = (await res.json().catch(() => null)) as { error?: string } | null;
    throw new Error(body?.error || `${what} (${res.status})`);
  }
}

function postJson(path: string, body: unknown): Promise<Response> {
  return authedFetch(path, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(body),
  });
}


/**
 * Mints an enrollment token for a new host. A dialed host also records the
 * address the controller will reach it at, because the controller has to
 * open the connection before that host can register.
 */
export async function enrollWorker(
  label: string,
  dialed?: { endpoint: string },
  bucketIds: readonly bigint[] = [],
): Promise<EnrollResult> {
  const res = await postJson("/api/workers/enroll", {
    label,
    ...(bucketIds.length > 0 ? { bucket_ids: bucketIds.map(Number) } : {}),
    ...(dialed ? { connect_mode: "accept", endpoint: dialed.endpoint } : {}),
  });
  await ensureOk(res, "worker enrollment failed");
  return (await res.json()) as EnrollResult;
}

/**
 * Rotates an existing host's credential and pinned key without changing
 * its id, so everything pointing at that host keeps pointing at it. That
 * hands the host to whichever machine redeems the token, so it takes the
 * same access token that removing the host outright does.
 */
export async function reenrollWorker(
  workerId: bigint,
  connection?: { connectMode: "dial" | "accept"; endpoint: string },
): Promise<EnrollResult> {
  const res = await postJson(`/api/workers/${workerId}/reenroll`, {
    ...(connection
      ? { connect_mode: connection.connectMode, endpoint: connection.endpoint }
      : {}),
  });
  await ensureOk(res, "worker re-enrollment failed");
  return (await res.json()) as EnrollResult;
}

/**
 * Applies a host's pending update without waiting for it to go idle. The
 * agents running there are restarted and resumed with their history, but
 * whatever turn each was in the middle of does not carry across.
 */
export async function updateWorkerNow(workerId: bigint): Promise<void> {
  const res = await postJson(`/api/workers/${workerId}/update`, {});
  await ensureOk(res, "host update failed");
}

export async function removeWorker(workerId: bigint): Promise<void> {
  const res = await authedFetch(`/api/workers/${workerId}`, { method: "DELETE" });
  await ensureOk(res, "worker removal failed");
}

/**
 * A replacementWorkerId takes the projects pinned to a Host this drops
 * from the bucket; without one they move to the bucket's new default.
 */
export async function setBucketWorker(
  bucketId: bigint,
  workerId: bigint,
  allowedWorkerIds?: bigint[],
  replacementWorkerId?: bigint,
): Promise<void> {
  const res = await postJson(`/api/buckets/${bucketId}/worker`, {
    worker_id: Number(workerId),
    ...(allowedWorkerIds ? { allowed_worker_ids: allowedWorkerIds.map(Number) } : {}),
    ...(replacementWorkerId === undefined ? {} : { replacement_worker_id: Number(replacementWorkerId) }),
  });
  await ensureOk(res, "setting bucket worker failed");
}

/** A null workerId clears the override so the project inherits its bucket. */
export async function setProjectWorker(projectId: bigint, workerId: bigint | null, allowedWorkerIds?: bigint[]): Promise<void> {
  const res = await postJson(`/api/projects/${projectId}/worker`, {
    worker_id: workerId === null ? null : Number(workerId),
    ...(allowedWorkerIds ? { allowed_worker_ids: allowedWorkerIds.map(Number) } : {}),
  });
  await ensureOk(res, "setting project worker failed");
}

/** A null path clears the entry so spawns on that worker fall back to the project path. */
export async function setProjectWorkerPath(projectId: bigint, workerId: bigint, path: string | null): Promise<void> {
  const res = await postJson(`/api/projects/${projectId}/worker-path`, {
    worker: workerId.toString(),
    path,
  });
  await ensureOk(res, "setting project worker path failed");
}

export async function setDefaultBucket(bucketId: bigint): Promise<void> {
  const res = await postJson(`/api/buckets/${bucketId}/default`, {});
  await ensureOk(res, "setting default bucket failed");
}
