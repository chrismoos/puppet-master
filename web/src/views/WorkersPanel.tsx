import { Fragment, useEffect, useState, type FormEvent } from "react";
import {
  controllerBase,
  controllerOrigin,
  controllerCommand,
  enrollWorker,
  listenCommand,
  normalizeControllerUrl,
  normalizeEndpoint,
  reenrollWorker,
  removeWorker,
  updateWorkerNow,
  type EnrollResult,
  WORKER_LOCATIONS,
  WORKER_LOCATION_ORDER,
  WORKER_TYPES,
  WORKER_TYPE_ORDER,
  PLATFORMS,
  offeredAt,
  platformOf,
  resolveWorkerType,
  typeFromRuntime,
  runsOn,
  installSteps,
  type Platform,
  type WorkerLocation,
  type WorkerType,
} from "../api/workers";
import { clipboardFailureReason } from "@puppet-master/client-core/ws/clipboard";
import { copyTextToClipboard } from "../clipboard";
import { fetchVersion, type BuildInfo } from "../api/auth";
import { ConfirmDialog } from "./ConfirmDialog";
import { LOCAL_WORKER_ID } from "@puppet-master/client-core/format";
import type { Bucket, Worker } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { useAppState } from "../state/hooks";
import {
  hostConnectionView,
  hostRuntimeView,
  isDialedHost,
  offlineHostReason,
  orderWorkers,
  resolveProjectWorkerId,
} from "@puppet-master/client-core/state/worker";
import { Pager } from "./Pager";
import { DEFAULT_PAGE_SIZE, paginate } from "./pagination";
import { SettingsDialog } from "./SettingsDialog";
import { SettingsError, SettingsPageHead, errorMessage } from "./settingsParts";

const COPIED_RESET_MS = 1_500;

export function controllerBuildVersion(
  build: Pick<BuildInfo, "version" | "gitRev"> | null,
): string | null {
  return build ? `${build.version}+${build.gitRev}` : null;
}

/**
 * How a Workers row presents the pm build a worker reported. Drift
 * against the controller's own build is advisory, so it highlights
 * rather than errors, and a worker that never reported a build reads
 * as unknown.
 */
export function workerVersionView(
  pmVersion: string,
  controllerVersion: string | null,
): { label: string; drift: boolean; actionable: boolean; title?: string } {
  if (!pmVersion) return { label: "pm version unknown", drift: false, actionable: false };
  if (controllerVersion === null || pmVersion === controllerVersion) {
    return { label: `pm ${pmVersion}`, drift: false, actionable: false };
  }
  // A worker follows the controller's release. Builds of one release differ
  // only in metadata the release channel cannot address, so that drift is
  // worth showing but nothing can be installed to resolve it.
  const actionable = releaseVersion(pmVersion) !== releaseVersion(controllerVersion);
  return {
    label: `pm ${pmVersion}`,
    drift: true,
    actionable,
    title: actionable
      ? `differs from controller pm ${controllerVersion}`
      : `built from a different commit than controller pm ${controllerVersion}; the release channel publishes one build per version, so there is nothing to install`,
  };
}

/** The release part of a build identity: `0.3.0` out of `0.3.0+abc1234`. */
function releaseVersion(buildVersion: string): string {
  return buildVersion.split("+")[0] ?? buildVersion;
}

/** Which end the operator wants to open the connection for a new worker. */
export type ConnectDirection = "worker-dials" | "controller-dials";

/** What an operator chose for a worker's connection, for either flow. */
export interface HostConnectionChoice {
  direction: ConnectDirection;
  /** What holds the worker, which decides whether the command launches a container. */
  workerType: WorkerType;
  /** Whether the worker shares the controller's machine, which decides the address it dials. */
  location: WorkerLocation;
  /** Null or empty until the operator edits it, which leaves the controller's own address. */
  controllerUrl: string | null;
  endpoint: string;
  /** The worker's label, which becomes its name on its own machine. */
  name: string;
  platform: Platform;
}

/**
 * The command an operator runs on a worker, for whichever end is opening the
 * connection. A worker the controller dials waits on the address it was given;
 * one that dials out needs the address it reaches the worker plane at, which
 * is a different listener from the web UI and never the browser's origin.
 */
export function hostCommand(
  choice: HostConnectionChoice,
  origin: string,
  token: string,
  hostPlanePort: number | null,
): string {
  const type = resolveWorkerType(choice.workerType, choice.platform, choice.location);
  const options = { name: choice.name, runtime: WORKER_TYPES[type].runtime };
  if (choice.direction === "controller-dials") {
    return listenCommand(
      normalizeEndpoint(choice.endpoint) ?? choice.endpoint.trim(),
      token,
      options,
    );
  }
  // A guest's name for its host only resolves on the controller's own
  // machine, so a remote worker dials the controller's address whatever
  // holds it.
  const controller =
    choice.location === "remote"
      ? normalizeControllerUrl(choice.controllerUrl ?? "") ??
        controllerOrigin(origin, hostPlanePort)
      : controllerOrigin(origin, hostPlanePort, WORKER_TYPES[type].guestHost);
  return controllerCommand(controller, token, options);
}

/** How a worker's stored connection seeds the re-enroll form. */
export function reenrollDefaults(
  worker: Pick<Worker, "connectMode" | "endpoint" | "name" | "platform" | "runtime">,
): HostConnectionChoice {
  // A host that has registered already reported what holds it and what
  // it runs on, so re-enrolling starts from its own answers. Defaulting
  // to "this machine" handed a containerized host a command that would
  // have run pm beside its container instead of in it.
  return {
    direction: isDialedHost(worker) ? "controller-dials" : "worker-dials",
    workerType: typeFromRuntime(worker.runtime ?? ""),
    // Nothing a worker reports says which machine it is on, and the
    // controller's own address works from either, while a guest name
    // resolves only on the controller's machine.
    location: "remote",
    controllerUrl: null,
    endpoint: worker.endpoint,
    name: worker.name,
    platform: platformOf(worker.platform ?? ""),
  };
}

export function projectCreationWorkerSelection(
  value: string,
  bucket: Pick<Bucket, "defaultWorkerId"> | undefined,
) {
  const override = value === "" ? undefined : BigInt(value);
  return {
    override,
    effective: resolveProjectWorkerId({ workerId: override }, bucket),
  };
}

const DIRECTIONS: ReadonlyArray<{ value: ConnectDirection; title: string; detail: string }> = [
  {
    value: "worker-dials",
    title: "Worker → Controller",
    detail: "Most common. Use this when the worker can reach the controller directly, such as over the same private network.",
  },
  {
    value: "controller-dials",
    title: "Controller → Worker",
    detail: "Good for cloud instances, where the controller connects to the worker.",
  },
];

/** A type's label as a standalone choice: "a Lima VM" reads as "Lima VM". */
function workerTypeChoiceLabel(type: WorkerType): string {
  const label = WORKER_TYPES[type].label.replace(/^an? /, "");
  return label.charAt(0).toUpperCase() + label.slice(1);
}

function isPending(worker: Worker): boolean {
  return worker.id !== LOCAL_WORKER_ID && worker.lastSeenAtUnixMs === undefined;
}

/** How a Workers row states a worker's status, and why when it is not online. */
export function workerStatusView(
  worker: Pick<Worker, "id" | "online" | "lastSeenAtUnixMs" | "connectMode" | "endpoint">,
): { label: string; tone: "ok" | "warn" | "bad"; reason?: string } {
  if (worker.online) return { label: "Online", tone: "ok" };
  if (worker.id === LOCAL_WORKER_ID) return { label: "Offline", tone: "bad" };
  if (worker.lastSeenAtUnixMs === undefined) {
    return { label: "Pending", tone: "warn", reason: "enroll command not run yet" };
  }
  return { label: "Offline", tone: "bad", reason: offlineHostReason(worker) };
}

/**
 * The workers a filter keeps, in the order given. The filter is one phrase
 * looked for in each thing the row shows: name, hostname, platform, runtime,
 * connection, status, and version.
 */
export function filterWorkers<T extends Worker>(
  workers: readonly T[],
  query: string,
  controllerVersion: string | null,
): T[] {
  const phrase = query.trim().toLowerCase().replace(/\s+/g, " ");
  if (phrase === "") return [...workers];
  return workers.filter((worker) => {
    const connection = hostConnectionView(worker);
    return [
      worker.name,
      worker.hostname,
      worker.id === LOCAL_WORKER_ID ? "this controller" : "",
      worker.platform || "unknown",
      hostRuntimeView(worker)?.label ?? "",
      connection ? connection.label : "built in",
      connection?.endpoint ?? "",
      workerStatusView(worker).label,
      workerVersionView(worker.pmVersion, controllerVersion).label,
    ].some((shown) => shown.toLowerCase().includes(phrase));
  });
}

const PLATFORM_ORDER: readonly Platform[] = ["linux", "macos"];

type CopyCommand = (key: string, command: string) => void;

function CommandLine({
  command,
  copyKey,
  copied,
  onCopy,
}: {
  command: string;
  copyKey: string;
  copied: string | null;
  onCopy: CopyCommand;
}) {
  return (
    <div className="ui-cmd">
      <code>{command}</code>
      <button type="button" className="btn" onClick={() => onCopy(copyKey, command)}>
        {copied === copyKey ? "Copied" : "Copy"}
      </button>
    </div>
  );
}

function DirectionChoice({
  name,
  value,
  onChange,
}: {
  name: string;
  value: ConnectDirection;
  onChange: (next: ConnectDirection) => void;
}) {
  return (
    <div className="ui-choice" role="radiogroup" aria-label="Which side opens the connection?">
      {DIRECTIONS.map((direction) => (
        <label key={direction.value}>
          <input
            type="radio"
            name={name}
            value={direction.value}
            checked={value === direction.value}
            onChange={() => onChange(direction.value)}
          />
          <span>
            <b>{direction.title}</b>
            <small>{direction.detail}</small>
          </span>
        </label>
      ))}
    </div>
  );
}

/** Where the worker runs, asked first: a local one always dials this controller, so only a remote one is asked which side connects. */
function LocationField({
  value,
  onChange,
}: {
  value: WorkerLocation;
  onChange: (next: WorkerLocation) => void;
}) {
  return (
    <div className="ui-field">
      <span className="label">Location</span>
      <div>
        <LocationChoice value={value} onChange={onChange} />
      </div>
      <span className="ui-hint">
        {value === "local"
          ? "On the same machine as the controller. A container or VM there reaches the controller through its runtime's name for that machine."
          : "On another machine. The command dials the controller's own address, whatever holds the worker, unless the controller dials it."}
      </span>
    </div>
  );
}

function LocationChoice({
  value,
  onChange,
}: {
  value: WorkerLocation;
  onChange: (next: WorkerLocation) => void;
}) {
  return (
    <div className="ui-seg" role="group" aria-label="Location">
      {WORKER_LOCATION_ORDER.map((location) => (
        <button
          key={location}
          type="button"
          data-location={location}
          title={`${WORKER_LOCATIONS[location].label} (${WORKER_LOCATIONS[location].detail})`}
          aria-pressed={value === location}
          onClick={() => onChange(location)}
        >
          {WORKER_LOCATIONS[location].label}
        </button>
      ))}
    </div>
  );
}

function WorkerTypeChoice({
  id,
  value,
  platform,
  location,
  onChange,
}: {
  id: string;
  value: WorkerType;
  platform: Platform;
  location: WorkerLocation;
  onChange: (next: WorkerType) => void;
}) {
  return (
    <select
      id={id}
      className="ui-select ui-w-sm"
      value={value}
      onChange={(event) => onChange(event.target.value as WorkerType)}
    >
      {WORKER_TYPE_ORDER.filter((type) => offeredAt(type, location)).map((type) => {
        const ok = runsOn(type, platform);
        return (
          <option key={type} value={type} disabled={!ok}>
            {workerTypeChoiceLabel(type)}
            {ok ? "" : ` (not on ${PLATFORMS[platform]})`}
          </option>
        );
      })}
    </select>
  );
}

function PlatformChoice({
  value,
  onChange,
}: {
  value: Platform;
  onChange: (next: Platform) => void;
}) {
  return (
    <div className="ui-seg" role="group" aria-label="Worker's operating system">
      {PLATFORM_ORDER.map((platform) => (
        <button
          key={platform}
          type="button"
          data-platform={platform}
          aria-pressed={value === platform}
          onClick={() => onChange(platform)}
        >
          {PLATFORMS[platform]}
        </button>
      ))}
    </div>
  );
}

const LOOPBACK_HOSTS = ["localhost", "127.0.0.1", "[::1]"];

/** The host of a controller URL when it only resolves on the controller's own machine. */
function loopbackHost(url: string): string | null {
  try {
    const host = new URL(url).hostname;
    return LOOPBACK_HOSTS.includes(host) ? host : null;
  } catch {
    return null;
  }
}

/**
 * What a worker that dials the controller is asked: where it is, what its
 * machine runs, what holds it, and for a remote one the address it dials.
 */
function DialOutFields({
  idPrefix,
  subject,
  location,
  platform,
  workerType,
  controllerUrl,
  origin,
  hostPlanePort,
  onChange,
}: {
  idPrefix: string;
  /** How the notes refer to the worker: its name, or "the worker". */
  subject: string;
  location: WorkerLocation;
  platform: Platform;
  workerType: WorkerType;
  controllerUrl: string | null;
  /** The controller's address as workers should reach it, before the worker-plane port. */
  origin: string;
  hostPlanePort: number | null;
  onChange: (
    patch: Partial<Pick<HostConnectionChoice, "platform" | "workerType" | "controllerUrl">>,
  ) => void;
}) {
  const type = resolveWorkerType(workerType, platform, location);
  const { runtime, guestHost } = WORKER_TYPES[type];
  const ownAddress = controllerOrigin(origin, hostPlanePort);
  const url = controllerUrl ?? ownAddress;
  const dialed = normalizeControllerUrl(url);
  const loopback = loopbackHost(dialed ?? ownAddress);
  return (
    <Fragment>
      <div className="ui-field">
        <span className="label">Worker's operating system</span>
        <div>
          <PlatformChoice value={platform} onChange={(next) => onChange({ platform: next })} />
        </div>
        <span className="ui-hint">
          It decides what can run there and what to install first, not the command, which is
          the same on both.
        </span>
      </div>
      <div className="ui-field">
        <label htmlFor={`${idPrefix}-type`}>Where it runs</label>
        <div>
          <WorkerTypeChoice
            id={`${idPrefix}-type`}
            value={type}
            platform={platform}
            location={location}
            onChange={(next) => onChange({ workerType: next })}
          />
        </div>
        <span className="ui-hint">What holds {subject} on its machine.</span>
        {runtime && (
          <span className="ui-hint">
            The command launches a {runtime} container and runs the worker inside it.
          </span>
        )}
        {location === "local" && guestHost && (
          <span className="ui-hint">Connects to this controller through {guestHost}.</span>
        )}
      </div>
      {location === "remote" && (
        <div className="ui-field">
          <label htmlFor={`${idPrefix}-controller-url`}>Controller URL</label>
          <input
            id={`${idPrefix}-controller-url`}
            className="ui-input ui-w-md"
            value={url}
            onChange={(e) => onChange({ controllerUrl: e.target.value })}
            placeholder={ownAddress}
          />
          <span className="ui-hint">
            How that machine reaches this controller's worker plane, which is a separate listener
            from this web UI. Edit it if the machine uses a different address, such as a tailnet or
            LAN IP.
          </span>
          {dialed === null && (
            <span className="ui-hint set-hint-warn">
              Not a full URL with a scheme, so the command uses {ownAddress} instead.
            </span>
          )}
          {loopback && (
            <span className="ui-hint set-hint-warn">
              {loopback} reaches the controller only from its own machine, so a remote worker
              needs an address it can route to.
            </span>
          )}
        </div>
      )}
    </Fragment>
  );
}

/** Said when Escape or a backdrop click is refused because a command is on screen. */
function ShownOnceNote() {
  return (
    <span className="ui-hint set-hint-warn set-footer-note" role="status">
      This command is shown only once. Copy it, then choose Done.
    </span>
  );
}

function InstallFirst({
  steps,
  copied,
  onCopy,
}: {
  steps: readonly string[];
  copied: string | null;
  onCopy: CopyCommand;
}) {
  if (steps.length === 0) return null;
  return (
    <div className="ui-field enroll-install">
      <span className="label">
        {steps.length > 1 ? "Nothing installed on that machine yet?" : "No pm on that machine yet?"}
      </span>
      {steps.map((step, index) => (
        <CommandLine key={step} command={step} copyKey={`install-${index}`} copied={copied} onCopy={onCopy} />
      ))}
      <span className="ui-hint">
        Run {steps.length > 1 ? "these" : "it"} first. The token above stays valid until it expires.
      </span>
    </div>
  );
}

export function WorkersPanel() {
  const state = useAppState();
  const [error, setError] = useState<string | null>(null);
  const [adding, setAdding] = useState(false);
  const [reenrolled, setReenrolled] = useState<ReenrollResult | null>(null);
  // Which worker is being re-enrolled, and what the operator chose for it.
  // Re-enrolling is when a worker can also move between connection modes or
  // change the address it is reached at, so it asks rather than assumes.
  const [reenrolling, setReenrolling] = useState<ReenrollDraft | null>(null);
  const [removing, setRemoving] = useState<Worker | null>(null);
  const [reenrollError, setReenrollError] = useState<string | null>(null);
  const [copied, setCopied] = useState<string | null>(null);
  const [build, setBuild] = useState<BuildInfo | null>(null);
  const [query, setQuery] = useState("");
  const [page, setPage] = useState(0);
  const [size, setSize] = useState<number>(DEFAULT_PAGE_SIZE);

  useEffect(() => {
    let live = true;
    void fetchVersion().then((b) => {
      if (live) setBuild(b);
    });
    return () => {
      live = false;
    };
  }, []);

  useEffect(() => {
    if (copied === null) return;
    const timer = setTimeout(() => setCopied(null), COPIED_RESET_MS);
    return () => clearTimeout(timer);
  }, [copied]);

  const workers = orderWorkers([...state.workers.values()]);
  const controllerVersion = controllerBuildVersion(build);
  const matching = filterWorkers(workers, query, controllerVersion);
  const shown = paginate(matching, page, size);
  const reenrollTarget = reenrolling
    ? workers.find((worker) => worker.id === reenrolling.workerId)
    : undefined;

  const run = (action: Promise<unknown>, then?: () => void) => {
    setError(null);
    action.then(then).catch((err: unknown) => setError(errorMessage(err)));
  };

  const openReenroll = (worker: Worker) => {
    setError(null);
    setReenrollError(null);
    setReenrolled(null);
    setCopied(null);
    setReenrolling({
      workerId: worker.id,
      choice: reenrollDefaults(worker),
      platform: platformOf(worker.platform || build?.platform || ""),
    });
  };

  const closeReenroll = () => {
    setReenrolling(null);
    setReenrolled(null);
    setReenrollError(null);
  };

  const reenroll = (worker: Worker, draft: ReenrollDraft) => {
    setReenrollError(null);
    // A type the chosen platform or location cannot use is never the one the
    // command is built for, as in the add form.
    const choice: HostConnectionChoice = {
      ...draft.choice,
      platform: draft.platform,
      workerType: resolveWorkerType(draft.choice.workerType, draft.platform, draft.choice.location),
    };
    const dialed = choice.direction === "controller-dials";
    const endpoint = normalizeEndpoint(choice.endpoint);
    if (dialed && endpoint === null) {
      setReenrollError("a Worker the controller dials needs a host:port to dial");
      return;
    }
    reenrollWorker(worker.id, {
      connectMode: dialed ? "accept" : "dial",
      endpoint: dialed ? (endpoint ?? "") : "",
    })
      .then((result) => {
        setReenrolled({ workerId: worker.id, result, choice, platform: draft.platform });
      })
      .catch((err: unknown) => setReenrollError(errorMessage(err)));
  };

  const copyCommand: CopyCommand = (key, command) => {
    copyTextToClipboard(command)
      .then(() => setCopied(key))
      .catch((err: unknown) => setError(`could not copy to clipboard: ${clipboardFailureReason(err)}`));
  };

  const rotated = reenrolled && reenrolled.workerId === reenrollTarget?.id ? reenrolled : null;

  return (
    <section className="set-page wide" aria-labelledby="settings-page-title">
      <SettingsPageHead
        title="Workers"
        description="Machines that run agent sessions for this controller."
        actions={
          <button
            type="button"
            className="btn btn-primary btn-lg"
            aria-haspopup="dialog"
            onClick={() => {
              setCopied(null);
              setAdding(true);
            }}
          >
            Add worker
          </button>
        }
      />
      <SettingsError message={error} onDismiss={() => setError(null)} />
      {workers.length === 0 ? (
        <div className="ui-empty">
          <b>No workers registered</b>
          <p>The local worker is disabled for this daemon. Add a worker to run agent sessions.</p>
        </div>
      ) : (
        <Fragment>
          <div className="set-list-tools">
            <input
              type="search"
              className="ui-input ui-w-md"
              placeholder="Filter by name, platform, status…"
              aria-label="Filter workers"
              value={query}
              onChange={(e) => {
                setQuery(e.target.value);
                setPage(0);
              }}
            />
            {query.trim() !== "" && (
              <span className="ui-hint" role="status">
                {matching.length} of {workers.length}
              </span>
            )}
          </div>
          {matching.length === 0 ? (
            <div className="ui-empty set-workers-empty">
              <b>No worker matches that filter</b>
              <p>
                The filter looks at each worker's name, hostname, platform, runtime, connection,
                status, and version.
              </p>
              <div>
                <button type="button" className="btn" onClick={() => setQuery("")}>
                  Clear filter
                </button>
              </div>
            </div>
          ) : (
            <div className="ui-list set-table-wrap">
              <table className="ui-grid set-workers">
                <thead>
                  <tr>
                    <th>Worker</th>
                    <th>Status</th>
                    <th>Platform</th>
                    <th>Connection</th>
                    <th>Version</th>
                    <th><span className="visually-hidden">Actions</span></th>
                  </tr>
                </thead>
                <tbody>
                  {shown.rows.map((worker) => {
                    const local = worker.id === LOCAL_WORKER_ID;
                    const version = workerVersionView(worker.pmVersion, controllerVersion);
                    const connection = hostConnectionView(worker);
                    const runtime = hostRuntimeView(worker);
                    const status = workerStatusView(worker);
                    return (
                      <tr
                        className="worker-row"
                        key={worker.id.toString()}
                        data-worker-id={worker.id.toString()}
                        data-pending={isPending(worker) ? "" : undefined}
                      >
                        <td data-label="Worker">
                          <b className="worker-name">{worker.name}</b>
                          <div className="sub">
                            {local
                              ? [worker.hostname, "this controller"].filter(Boolean).join(" · ")
                              : worker.hostname}
                          </div>
                        </td>
                        <td data-label="Status">
                          <span className="worker-status" data-status={status.label.toLowerCase()}>
                            <span className={`ui-dot ${status.tone}`} aria-hidden="true" /> {status.label}
                          </span>
                          {status.reason && <div className="sub">{status.reason}</div>}
                        </td>
                        <td data-label="Platform" className="sub">
                          <span className="worker-platform">{worker.platform || "unknown"}</span>
                          {runtime && (
                            <div className="worker-runtime" title={runtime.title}>{runtime.label}</div>
                          )}
                        </td>
                        <td data-label="Connection" className="sub">
                          {connection ? (
                            <span className="worker-connect-mode" title={connection.title}>
                              {connection.label}
                              {connection.endpoint && <code>{connection.endpoint}</code>}
                            </span>
                          ) : <span>built in</span>}
                        </td>
                        <td data-label="Version" className="sub">
                          <span
                            className={version.drift ? "worker-version drift" : "worker-version"}
                            title={version.title}
                          >
                            {version.label}
                          </span>
                        </td>
                        <td className="right">
                          {!local && (
                            <Fragment>
                              {version.actionable && (
                                <button
                                  type="button"
                                  className="btn btn-quiet"
                                  title="Update this Worker now instead of waiting for it to go idle. Agents running there are restarted and resumed with their history, but a turn in flight is lost and each session comes back waiting for input"
                                  onClick={() => {
                                    if (
                                      window.confirm(
                                        `Update ${worker.name} now? Its agents are restarted and resumed with their history, but any turn in flight is lost and each session comes back waiting for input.`,
                                      )
                                    ) {
                                      run(updateWorkerNow(worker.id));
                                    }
                                  }}
                                >
                                  Update now
                                </button>
                              )}
                              <button
                                type="button"
                                className="btn btn-quiet"
                                aria-haspopup="dialog"
                                title="Rotate this Worker's credential and pinned key, keeping the Worker itself"
                                onClick={() => openReenroll(worker)}
                              >
                                Re-enroll
                              </button>
                              <button
                                type="button"
                                className="btn btn-quiet btn-quiet-danger"
                                aria-haspopup="dialog"
                                onClick={() => setRemoving(worker)}
                              >
                                Remove
                              </button>
                            </Fragment>
                          )}
                        </td>
                      </tr>
                    );
                  })}
                </tbody>
              </table>
              {matching.length > DEFAULT_PAGE_SIZE && (
                <Pager
                  page={shown}
                  onPage={setPage}
                  size={size}
                  onSize={(next) => {
                    setSize(next);
                    setPage(0);
                  }}
                  noun="workers"
                />
              )}
            </div>
          )}
        </Fragment>
      )}

      {reenrolling && reenrollTarget && (
        <SettingsDialog
          labelledBy={REENROLL_TITLE_ID}
          guarded={rotated !== null}
          onClose={closeReenroll}
        >
          {(refused) => (
            <ReenrollForm
              worker={reenrollTarget}
              draft={reenrolling}
              rotated={rotated}
              command={
                rotated
                  ? hostCommand(
                      rotated.choice,
                      controllerBase(build, window.location.origin),
                      rotated.result.token,
                      build?.hostPlanePort ?? null,
                    )
                  : ""
              }
              build={build}
              error={reenrollError}
              refused={refused}
              copied={copied}
              onCopy={copyCommand}
              onChange={setReenrolling}
              onSubmit={(draft) => reenroll(reenrollTarget, draft)}
              onClose={closeReenroll}
            />
          )}
        </SettingsDialog>
      )}

      {removing && (
        <ConfirmDialog
          title="remove worker"
          titleId="worker-remove-title"
          confirmLabel="remove worker"
          busyLabel="removing…"
          onClose={() => setRemoving(null)}
          onConfirm={async () => {
            await removeWorker(removing.id);
            setRemoving(null);
          }}
        >
          <p>Remove “{removing.name}”?</p>
          <p className="muted-line">
            Its enrollment is revoked and the machine has to enroll again to come back. Sessions running there end.
          </p>
        </ConfirmDialog>
      )}

      {adding && (
        <AddWorkerDialog
          build={build}
          workers={workers}
          buckets={[...state.buckets.values()]}
          copied={copied}
          onCopy={copyCommand}
          onClose={() => setAdding(false)}
        />
      )}
    </section>
  );
}

const REENROLL_TITLE_ID = "reenroll-worker-title";
const ADD_WORKER_TITLE_ID = "add-worker-title";

/** What the operator is choosing while re-enrolling one worker. */
interface ReenrollDraft {
  workerId: bigint;
  choice: HostConnectionChoice;
  /** The worker machine's operating system, which decides what can run there and what to install first. */
  platform: Platform;
}

/** A minted re-enrollment and the choices its command was built from. */
interface ReenrollResult {
  workerId: bigint;
  result: EnrollResult;
  choice: HostConnectionChoice;
  platform: Platform;
}

/**
 * Re-enrolling is when a worker can also move between connection modes or
 * change the address it is reached at, so it asks the same questions as adding
 * one and shows the command beside them.
 */
function ReenrollForm({
  worker,
  draft,
  rotated,
  command,
  build,
  error,
  refused,
  copied,
  onCopy,
  onChange,
  onSubmit,
  onClose,
}: {
  worker: Worker;
  draft: ReenrollDraft;
  rotated: ReenrollResult | null;
  command: string;
  build: BuildInfo | null;
  error: string | null;
  refused: boolean;
  copied: string | null;
  onCopy: CopyCommand;
  onChange: (next: ReenrollDraft) => void;
  onSubmit: (draft: ReenrollDraft) => void;
  onClose: () => void;
}) {
  const { choice, platform } = draft;
  const setChoice = (patch: Partial<HostConnectionChoice>) =>
    onChange({ ...draft, choice: { ...choice, ...patch } });
  const installFirst = !rotated
    ? []
    : installSteps(
        rotated.choice.direction === "controller-dials" ? "machine" : rotated.choice.workerType,
        rotated.platform,
        build?.installCommand ?? "",
      );
  return (
    <form
      className="ui-form-card set-reenroll"
      onSubmit={(e) => {
        e.preventDefault();
        onSubmit(draft);
      }}
    >
      <header>
        <h3 id={REENROLL_TITLE_ID}>Re-enroll {worker.name}</h3>
        <span className="ui-hint">
          The machine that redeems this command becomes {worker.name}, keeping its project paths and
          history.
        </span>
      </header>
      <div className="body ui-cols">
        <div>
          {error && <div className="form-error" role="alert">{error}</div>}
          <LocationField
            value={choice.location}
            onChange={(location) =>
              setChoice(
                location === "local" ? { location, direction: "worker-dials" } : { location },
              )
            }
          />
          {choice.location === "remote" && (
            <div className="ui-field">
              <span className="label">Which side opens the connection?</span>
              <DirectionChoice
                name={`reenroll-direction-${worker.id}`}
                value={choice.direction}
                onChange={(direction) => setChoice({ direction })}
              />
            </div>
          )}
          {choice.direction === "worker-dials" ? (
            <DialOutFields
              idPrefix={`reenroll-${worker.id}`}
              subject={worker.name}
              location={choice.location}
              platform={platform}
              workerType={choice.workerType}
              controllerUrl={choice.controllerUrl}
              origin={controllerBase(build, window.location.origin)}
              hostPlanePort={build?.hostPlanePort ?? null}
              onChange={({ platform: nextPlatform, ...patch }) =>
                onChange({
                  ...draft,
                  platform: nextPlatform ?? platform,
                  choice: { ...choice, ...patch },
                })
              }
            />
          ) : (
            <div className="ui-field">
              <label htmlFor={`reenroll-endpoint-${worker.id}`}>Address the controller dials</label>
              <input
                id={`reenroll-endpoint-${worker.id}`}
                className="ui-input ui-w-md"
                value={choice.endpoint}
                onChange={(e) => setChoice({ endpoint: e.target.value })}
                placeholder="dmz-box.internal:7678"
              />
              <span className="ui-hint">
                The host:port this controller reaches {worker.name} at. Edit it to move the Worker to a
                new address.
              </span>
            </div>
          )}
        </div>
        <div>
          {rotated ? (
            <Fragment>
              <div className="ui-field enroll-result">
                <span className="label">Run this on {worker.name} to finish re-enrolling</span>
                <CommandLine
                  command={command}
                  copyKey={`reenroll-${worker.id}`}
                  copied={copied}
                  onCopy={onCopy}
                />
                <span className="ui-hint">
                  Shown once. This rotates {worker.name}'s credential and pinned key in place,
                  which is how an existing Worker moves onto mutual TLS. The Worker keeps its id,
                  so its bucket allowlists, project overrides, per-Worker paths, and session
                  history all survive, unlike removing and re-adding it. The token expires{" "}
                  {new Date(rotated.result.expiresAtUnixMs).toLocaleString()}.
                </span>
                {rotated.choice.direction === "controller-dials" &&
                  normalizeEndpoint(rotated.choice.endpoint) !== worker.endpoint && (
                    <span className="ui-hint">
                      This Worker still reads as {worker.endpoint || "not dialed"} until it
                      completes the command above, because that is where the controller can
                      reach it today. Abandoning a re-enrollment therefore leaves a working
                      Worker working.
                    </span>
                  )}
              </div>
              <InstallFirst steps={installFirst} copied={copied} onCopy={onCopy} />
            </Fragment>
          ) : (
            <div className="ui-empty set-cmd-placeholder">
              <b>The command appears here</b>
              <p>
                It carries a one-time token and rotates {worker.name}'s credential, so it is generated
                when you ask for it and shown once.
              </p>
            </div>
          )}
        </div>
      </div>
      <footer>
        <button type="submit" className="btn btn-primary btn-lg">
          {rotated ? "Generate new command" : "Generate command"}
        </button>
        <button type="button" className="btn btn-quiet btn-lg" onClick={onClose}>
          {rotated ? "Done" : "Cancel"}
        </button>
        {refused && <ShownOnceNote />}
      </footer>
    </form>
  );
}

interface Enrollment {
  result: EnrollResult;
  name: string;
}

type AddWorkerProps = {
  build: BuildInfo | null;
  workers: readonly Worker[];
  buckets?: readonly Bucket[];
  copied: string | null;
  onCopy: CopyCommand;
  onClose: () => void;
};

function AddWorkerDialog(props: AddWorkerProps) {
  const [minted, setMinted] = useState(false);
  return (
    <SettingsDialog labelledBy={ADD_WORKER_TITLE_ID} guarded={minted} onClose={props.onClose}>
      {(refused) => <AddWorkerForm {...props} refused={refused} onMinted={setMinted} />}
    </SettingsDialog>
  );
}

export function AddWorkerForm({
  build,
  workers,
  buckets = [],
  copied,
  onCopy,
  onClose,
  refused = false,
  onMinted,
}: AddWorkerProps & {
  /** Whether a dismissal was just refused because a command is on screen. */
  refused?: boolean;
  /** Told whether a command is on screen, which is when closing needs the Done button. */
  onMinted?: (minted: boolean) => void;
}) {
  const [selectedBucketIds, setSelectedBucketIds] = useState<bigint[]>([]);
  const [generating, setGenerating] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [name, setName] = useState("");
  const [workerType, setWorkerType] = useState<WorkerType>("machine");
  const [location, setLocation] = useState<WorkerLocation>("local");
  // Seeded from the controller's own OS, because a Worker on this
  // machine is the common case and shares it.
  const [platform, setPlatform] = useState<Platform | null>(null);
  const [enrolled, setEnrolled] = useState<Enrollment | null>(null);
  const [controllerUrl, setControllerUrl] = useState<string | null>(null);
  const [direction, setDirection] = useState<ConnectDirection>("worker-dials");
  const [endpoint, setEndpoint] = useState("");

  const normalizedEndpoint = normalizeEndpoint(endpoint);
  const dialedHost = direction === "controller-dials";
  const effectivePlatform: Platform = platform ?? platformOf(build?.platform ?? "");
  // A type its platform cannot run, or its location does not offer, is never
  // the one that gets enrolled, so a change moves off it rather than
  // generating a command that machine could not run.
  const effectiveType = resolveWorkerType(workerType, effectivePlatform, location);
  const installFirst = installSteps(
    dialedHost ? "machine" : effectiveType,
    effectivePlatform,
    build?.installCommand ?? "",
  );
  // The worker the last command enrolled, while it is still waiting for
  // that command to run: asking again under the same name replaces its
  // token rather than adding a second worker of that name.
  const waiting = enrolled && name.trim() === enrolled.name
    ? workers.find((worker) => worker.name === enrolled.name && isPending(worker))
    : undefined;

  const mint = (next: Enrollment | null) => {
    setEnrolled(next);
    onMinted?.(next !== null);
  };

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (generating) return;
    setError(null);
    const label = name.trim();
    if (label === "") {
      setError("a Worker needs a name: it names the Worker on its own machine");
      return;
    }
    if (dialedHost && normalizedEndpoint === null) {
      setError("a Worker the controller dials needs an address to dial, as host:port");
      return;
    }
    setGenerating(true);
    const minted = waiting
      ? reenrollWorker(waiting.id, {
          connectMode: dialedHost ? "accept" : "dial",
          endpoint: dialedHost ? (normalizedEndpoint ?? "") : "",
        })
      : enrollWorker(
          label,
          normalizedEndpoint !== null && dialedHost ? { endpoint: normalizedEndpoint } : undefined,
          selectedBucketIds,
        );
    minted
      .then((result) => mint({ result, name: label }))
      .catch((err: unknown) => {
        mint(null);
        setError(errorMessage(err));
      })
      .finally(() => setGenerating(false));
  };

  const command = !enrolled
    ? ""
    : hostCommand(
        {
          direction,
          workerType: effectiveType,
          location,
          controllerUrl,
          endpoint,
          name: enrolled.name,
          platform: effectivePlatform,
        },
        controllerBase(build, window.location.origin),
        enrolled.result.token,
        build?.hostPlanePort ?? null,
      );

  return (
    <form className="ui-form-card set-add-worker" onSubmit={submit}>
      <header>
        <h3 id={ADD_WORKER_TITLE_ID}>Add a worker</h3>
        <span className="ui-hint">Answer a few questions, then run one command on that machine.</span>
      </header>
      <div className="body ui-cols">
        <div>
          {error && <div className="form-error" role="alert">{error}</div>}
          <LocationField
            value={location}
            onChange={(next) => {
              setLocation(next);
              if (next === "local") setDirection("worker-dials");
            }}
          />
          {location === "remote" && (
            <div className="ui-field">
              <span className="label">Which side opens the connection?</span>
              <DirectionChoice name="add-worker-direction" value={direction} onChange={setDirection} />
            </div>
          )}
          {dialedHost ? (
            <div className="ui-field">
              <label htmlFor="add-worker-endpoint">Worker address</label>
              <input
                id="add-worker-endpoint"
                className="ui-input ui-w-md"
                value={endpoint}
                onChange={(e) => setEndpoint(e.target.value)}
                placeholder="10.0.0.5:7677"
              />
              <span className="ui-hint">
                The host:port the controller dials to reach this worker, and the address the worker
                listens on.
              </span>
              {endpoint.trim() !== "" && normalizedEndpoint === null && (
                <span className="ui-hint set-hint-warn">Not a host:port address.</span>
              )}
            </div>
          ) : (
            <DialOutFields
              idPrefix="add-worker"
              subject="the worker"
              location={location}
              platform={effectivePlatform}
              workerType={workerType}
              controllerUrl={controllerUrl}
              origin={controllerBase(build, window.location.origin)}
              hostPlanePort={build?.hostPlanePort ?? null}
              onChange={(patch) => {
                if (patch.platform !== undefined) setPlatform(patch.platform);
                if (patch.workerType !== undefined) setWorkerType(patch.workerType);
                if (patch.controllerUrl !== undefined) setControllerUrl(patch.controllerUrl);
              }}
            />
          )}
          <div className="ui-field">
            <label htmlFor="add-worker-name">Name</label>
            <input
              id="add-worker-name"
              className="ui-input ui-w-sm"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="build-box"
            />
            <span className="ui-hint">Its name on its own machine, and the name of its container.</span>
          </div>
          <fieldset className="catalog-fieldset set-worker-buckets" disabled={generating || enrolled !== null}>
            <legend>Bucket Access</legend>
            <span className="ui-hint" id="add-worker-bucket-hint">
              Selecting a bucket lets all projects within it use this worker. For finer control,
              leave this empty and add the worker to specific buckets or projects afterward.
            </span>
            {buckets.length === 0 ? (
              <span className="ui-hint">No buckets yet. You can configure access after creating one.</span>
            ) : (
              <details>
                <summary>{selectedBucketIds.length === 0 ? "Select buckets (optional)" : `${selectedBucketIds.length} selected`}</summary>
                <div className="ui-list" aria-describedby="add-worker-bucket-hint">
                  {buckets.map((bucket) => (
                    <label className="catalog-check" key={bucket.id.toString()}>
                      <input
                        type="checkbox"
                        checked={selectedBucketIds.includes(bucket.id)}
                        onChange={(event) => setSelectedBucketIds((current) => event.target.checked
                          ? [...current, bucket.id]
                          : current.filter((id) => id !== bucket.id))}
                      />
                      <span>{bucket.name}</span>
                    </label>
                  ))}
                </div>
              </details>
            )}
          </fieldset>
        </div>
        <div>
          {enrolled ? (
            <Fragment>
              <div className="ui-field enroll-result">
                <span className="label">Run this on the worker</span>
                <CommandLine command={command} copyKey="enroll" copied={copied} onCopy={onCopy} />
                <span className="ui-hint">
                  Shown once. Works for a single enrollment and expires{" "}
                  {new Date(enrolled.result.expiresAtUnixMs).toLocaleString()}.
                </span>
              </div>
              <InstallFirst steps={installFirst} copied={copied} onCopy={onCopy} />
            </Fragment>
          ) : (
            <div className="ui-empty set-cmd-placeholder">
              <b>The command appears here</b>
              <p>
                It carries a one-time token, so it is generated when you ask for it and shown once.
              </p>
            </div>
          )}
        </div>
      </div>
      <footer>
        <button type="submit" className="btn btn-primary btn-lg" disabled={generating}>
          {waiting ? "Generate new command" : "Generate command"}
        </button>
        <button type="button" className="btn btn-quiet btn-lg" onClick={onClose}>
          {enrolled ? "Done" : "Cancel"}
        </button>
        {refused ? (
          <ShownOnceNote />
        ) : (
          enrolled && (
            <span className="ui-hint set-footer-note">
              {enrolled.name} appears in the list as Pending until the command runs.
            </span>
          )
        )}
      </footer>
    </form>
  );
}
