import { useHarnessInstall } from "../components/HarnessInstall";
import { InstallCancelled } from "../state/harnessInstall";
import { useEffect, useReducer, useState, type FormEvent } from "react";
import { DirectoryPicker } from "../components/DirectoryPicker";
import { LOCAL_WORKER_ID } from "@puppet-master/client-core/format";
import { AgentKind, PermissionMode, SessionRole } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { useAppState, useClient } from "../state/hooks";
import { permissionModeLabel, resolvePermissionMode } from "@puppet-master/client-core/state/permission";
import { agentFromValue, agentLabel, agentValue, resolveAgent } from "@puppet-master/client-core/state/agent";
import { AgentSelect } from "../components/AgentSelect";
import {
  coveredAgents,
  profileApplies,
  resolveModelProfile,
  selectEndpoint,
} from "@puppet-master/client-core/state/modelProfile";
import { defaultSpawnCwd, initializeSpawnCwd, spawnCwdReducer } from "@puppet-master/client-core/state/spawnCwd";
import { defaultSpawnProjectId, readRememberedSpawnProject } from "../state/supervisorSpawn";
import { submitSpawn } from "../state/spawnSubmit";
import { orderWorkers, resolveProjectWorkerId, workerUnavailableReason } from "@puppet-master/client-core/state/worker";
import { blocksSpawn } from "../api/projectHost";
import { useProjectHost } from "../api/useProjectHost";

/** Prefilled fields, e.g. from a work item or a pm:spawn link. */
export interface SpawnPrefill {
  title?: string;
  prompt?: string;
  /** Item to mark in-progress and link once the session spawns. */
  itemId?: bigint;
  /** Owning bucket for itemId; item numbers are never globally addressable. */
  itemBucketId?: bigint;
}

export function SpawnDialog({
  onClose,
  onCreated,
  defaultProjectId,
  lockProject = false,
  bucketId,
  prefill,
}: {
  onClose: () => void;
  onCreated: (id: string) => void;
  defaultProjectId?: string;
  lockProject?: boolean;
  /** Scopes the dialog to one bucket: its projects only, home project defaulted. */
  bucketId?: string;
  prefill?: SpawnPrefill;
}) {
  const client = useClient();
  const state = useAppState();
  const workerForProject = (id: string): bigint => {
    const project = state.projects.get(id);
    const bucket = project ? state.buckets.get(project.bucketId.toString()) : undefined;
    return resolveProjectWorkerId(project, bucket);
  };
  const initialProjectId = bucketId !== undefined
    ? defaultSpawnProjectId(
        bucketId,
        state.projects.values(),
        readRememberedSpawnProject(bucketId, SessionRole.WORKER),
      )
    : (defaultProjectId ?? "");
  const initialWorkerId = initialProjectId ? workerForProject(initialProjectId) : LOCAL_WORKER_ID;
  const initialProject = initialProjectId ? state.projects.get(initialProjectId) : undefined;
  const [projectId, setProjectId] = useState(initialProjectId);
  const [agentOverride, setAgentOverride] = useState<AgentKind | undefined>();
  const [profileOverride, setProfileOverride] = useState("");
  const [title, setTitle] = useState(prefill?.title ?? "");
  const [prompt, setPrompt] = useState(prefill?.prompt ?? "");
  const [cwdState, dispatchCwd] = useReducer(
    spawnCwdReducer,
    defaultSpawnCwd(initialProject, initialWorkerId),
    initializeSpawnCwd,
  );
  const [permMode, setPermMode] = useState<PermissionMode>(PermissionMode.UNSPECIFIED);
  const [itemsApi, setItemsApi] = useState(true);
  const [role, setRole] = useState<SessionRole>(SessionRole.WORKER);
  const [workerId, setWorkerId] = useState<bigint>(initialWorkerId);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const harness = useHarnessInstall();

  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") onClose();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [onClose]);

  const buckets = [...state.buckets.values()].sort(
    (a, b) => a.position - b.position || a.name.localeCompare(b.name),
  );
  const projects = [...state.projects.values()]
    .filter((project) => prefill?.itemBucketId === undefined || project.bucketId === prefill.itemBucketId)
    .filter((project) => bucketId === undefined || project.bucketId.toString() === bucketId)
    .sort((a, b) => a.name.localeCompare(b.name));
  const hasProjects = projects.length > 0;
  const selectedProject = projectId ? state.projects.get(projectId) : undefined;
  const selectedBucket = selectedProject
    ? state.buckets.get(selectedProject.bucketId.toString())
    : undefined;
  const inheritedAgent = resolveAgent(selectedProject, selectedBucket);
  const effectiveAgent = resolveAgent(selectedProject, selectedBucket, agentOverride);
  const inheritedProfile = resolveModelProfile(selectedProject, selectedBucket);
  const effectiveProfile = resolveModelProfile(
    selectedProject,
    selectedBucket,
    profileOverride ? BigInt(profileOverride) : undefined,
  );
  const effectiveProfileRecord = effectiveProfile
    ? state.modelProfiles.get(effectiveProfile.profileId.toString())
    : undefined;
  // The daemon rejects a spawn whose profile has no entry the agent can
  // speak to, so say so here rather than after the failure.
  const profileEndpoint = selectEndpoint(
    effectiveProfileRecord,
    effectiveAgent.agent,
    state.agentDialects,
  );
  const effectiveMode = resolvePermissionMode(
    permMode,
    selectedProject?.permissionMode ?? PermissionMode.UNSPECIFIED,
    selectedBucket?.permissionMode ?? PermissionMode.UNSPECIFIED,
  );
  const bypassing = effectiveMode === PermissionMode.BYPASS;

  const workers = orderWorkers([...state.workers.values()]).filter(
    (worker) => !selectedProject || selectedProject.allowedWorkerIds.includes(worker.id),
  );
  const selectedWorker = state.workers.get(workerId.toString());
  const projectHost = useProjectHost(selectedProject?.id, workerId, selectedWorker?.online ?? false);
  // A reachable host with an unusable project path is a config problem,
  // not an outage, so its own sentence wins over the generic one.
  const unavailableReason = blocksSpawn(projectHost)
    ? projectHost!.detail
    : workerUnavailableReason(state.workers, workerId);
  const defaultCwd = defaultSpawnCwd(selectedProject, workerId);
  const cwd = cwdState.value;

  useEffect(() => {
    dispatchCwd({ type: "default-changed", value: defaultCwd });
  }, [defaultCwd]);

  const submit = (e: FormEvent) => {
    e.preventDefault();
    if (busy) return;
    if (!projectId) {
      setError("pick a project");
      return;
    }
    if (unavailableReason) {
      setError(unavailableReason);
      return;
    }
    setBusy(true);
    setError(null);
    // Untouched means "use the effective project default". This preserves
    // the distinction between a displayed default and an explicit override
    // all the way to the daemon for both local and remote workers.
    harness.prepare({
      projectId,
      agent: agentOverride,
      title,
      prompt,
      cwdOverride: cwdState.touched ? cwd.trim() : "",
      permissionMode: permMode,
      workerId,
      itemsApi,
      role,
      modelProfileId: profileOverride ? BigInt(profileOverride) : undefined,
      spawnBucketId: bucketId,
      item: prefill?.itemId !== undefined && prefill.itemBucketId !== undefined
        ? { id: prefill.itemId, bucketId: prefill.itemBucketId }
        : undefined,
    })
      .then((request) => submitSpawn(client, request))
      .then((createdId) => {
        onClose();
        if (createdId !== undefined) onCreated(createdId.toString());
      })
      .catch((err: unknown) => {
        if (!(err instanceof InstallCancelled) && !(err instanceof DOMException && err.name === "AbortError")) {
          setError(err instanceof Error ? err.message : String(err));
        }
        setBusy(false);
      });
  };

  return (
    <div className="modal-backdrop" onClick={onClose}>
      <form className="modal" onClick={(e) => e.stopPropagation()} onSubmit={submit}>
        <h2 className="modal-title">
          {bucketId !== undefined
            ? `new session in ${state.buckets.get(bucketId)?.name ?? "bucket"}`
            : "spawn session"}
        </h2>
        {!hasProjects && (
          <p className="muted-line">
            no projects yet — create one under{" "}
            <a href="#/settings/projects" onClick={onClose}>
              Settings
            </a>{" "}
            first
          </p>
        )}
        {state.workers.size === 0 && (
          <p className="form-error">no workers registered; local worker is disabled for this daemon</p>
        )}
        <label className="field">
          <span className="field-label">project</span>
          <select
            autoFocus={bucketId === undefined}
            value={projectId}
            onChange={(e) => {
              const nextProjectId = e.target.value;
              const nextWorkerId = workerForProject(nextProjectId);
              setProjectId(nextProjectId);
              setWorkerId(nextWorkerId);
              dispatchCwd({
                type: "selection-changed",
                value: defaultSpawnCwd(state.projects.get(nextProjectId), nextWorkerId),
              });
            }}
            required
            disabled={!hasProjects || lockProject}
          >
            <option value="" disabled>
              pick a project…
            </option>
            {buckets.map((bucket) => {
              const inBucket = projects.filter((p) => p.bucketId === bucket.id);
              if (inBucket.length === 0) return null;
              return (
                <optgroup key={bucket.id.toString()} label={bucket.name}>
                  {inBucket.map((p) => (
                    <option key={p.id.toString()} value={p.id.toString()}>
                      {p.name}
                    </option>
                  ))}
                </optgroup>
              );
            })}
          </select>
        </label>
        {(workers.length > 1 || !selectedWorker) && (
          <label className="field">
            <span className="field-label">worker</span>
            <select
              value={workerId.toString()}
              onChange={(e) => {
                const nextWorkerId = BigInt(e.target.value);
                setWorkerId(nextWorkerId);
                dispatchCwd({
                  type: "selection-changed",
                  value: defaultSpawnCwd(selectedProject, nextWorkerId),
                });
              }}
            >
              {!selectedWorker && (
                <option value={workerId.toString()} disabled>
                  {workerId === LOCAL_WORKER_ID ? "local (disabled)" : `worker ${workerId} (unavailable)`}
                </option>
              )}
              {workers.map((w) => (
                <option key={w.id.toString()} value={w.id.toString()}>
                  {w.id === LOCAL_WORKER_ID ? `${w.name} (local)` : w.name}
                  {w.online ? "" : " — offline"}
                </option>
              ))}
            </select>
          </label>
        )}
        {unavailableReason && state.workers.size > 0 && <span className="field-warn">{unavailableReason}</span>}
        <label className="field">
          <span className="field-label">working directory</span>
          <DirectoryPicker
            value={cwd}
            onChange={(v) => {
              dispatchCwd({ type: "edit", value: v });
            }}
            placeholder={defaultCwd || "/home/you/src/project"}
            workerId={workerId}
          />
        </label>
        <label className="field">
          <span className="field-label">agent</span>
          <AgentSelect
            value={agentValue(agentOverride)}
            onChange={(value) => setAgentOverride(agentFromValue(value))}
            inheritLabel={`${agentLabel(inheritedAgent.agent)} — ${inheritedAgent.source} default`}
          />
          <span className="field-note" data-testid="effective-agent-source">
            Effective: {agentLabel(effectiveAgent.agent)} · {effectiveAgent.source === "explicit" ? "this spawn override" : `${effectiveAgent.source} default`}
          </span>
        </label>
        {state.modelProfiles.size > 0 && (
        <label className="field">
          <span className="field-label">model profile</span>
          {/* An explicit name keeps profile names, which are user data,
              out of this control's accessible name. */}
          <select
            aria-label="model profile"
            value={profileOverride}
            onChange={(e) => setProfileOverride(e.target.value)}
            data-testid="spawn-model-profile"
          >
            <option value="">
              {inheritedProfile
                ? `${state.modelProfiles.get(inheritedProfile.profileId.toString())?.name ?? `profile ${inheritedProfile.profileId}`} — ${inheritedProfile.source} default`
                : "agent account — no profile"}
            </option>
            {[...state.modelProfiles.values()]
              .sort((a, b) => a.name.localeCompare(b.name))
              .map((profile) => (
                <option key={profile.id.toString()} value={profile.id.toString()}>
                  {profile.name} — covers {coveredAgents(profile, state.agentDialects).map(agentLabel).join(", ") || "no agent"}
                </option>
              ))}
          </select>
          <span className="field-note" data-testid="effective-model-profile">
            {!effectiveProfileRecord
              ? `Effective: ${agentLabel(effectiveAgent.agent)}'s own account`
              : profileEndpoint
                ? `Effective: ${effectiveProfileRecord.name} · ${profileEndpoint.model}${effectiveProfile?.source === "explicit" ? " · this spawn override" : ` · ${effectiveProfile?.source} default`}`
                : profileApplies(effectiveAgent.agent, state.agentDialects)
                  ? `${effectiveProfileRecord.name} has no endpoint ${agentLabel(effectiveAgent.agent)} can use — this spawn will be rejected`
                  : `Effective: ${agentLabel(effectiveAgent.agent)}'s own account — no profile applies to it`}
          </span>
        </label>
        )}
        <label className="field">
          <span className="field-label">role</span>
          <select value={role} onChange={(e) => { const next=Number(e.target.value) as SessionRole; setRole(next); if(next===SessionRole.SUPERVISOR)setItemsApi(true); }}>
            <option value={SessionRole.WORKER}>Worker — execute tasks in this project</option>
            <option value={SessionRole.SUPERVISOR}>Supervisor — manage this bucket and spawn Workers</option>
          </select>
          {role === SessionRole.SUPERVISOR && <span className="field-note">bucket-level authority; launches here using this project&apos;s worker, cwd, and permissions</span>}
        </label>
        <label className="field">
          <span className="field-label">permission mode</span>
          <select
            value={permMode}
            onChange={(e) => setPermMode(Number(e.target.value) as PermissionMode)}
            className={bypassing ? "select-danger" : ""}
          >
            <option value={PermissionMode.UNSPECIFIED}>inherit (project default)</option>
            <option value={PermissionMode.DEFAULT}>default</option>
            <option value={PermissionMode.AUTO}>auto</option>
            <option value={PermissionMode.BYPASS}>bypass (dangerous)</option>
          </select>
          {permMode === PermissionMode.UNSPECIFIED && selectedProject && (
            <span className="field-note">
              effective: <span className={bypassing ? "pm-word-bypass" : ""}>
                {permissionModeLabel(effectiveMode)}
              </span>
            </span>
          )}
          {bypassing && (
            <span className="field-warn">bypass skips every permission check for this session</span>
          )}
        </label>
        <label className="field">
          <span className="field-label">title (optional)</span>
          <input
            value={title}
            onChange={(e) => setTitle(e.target.value)}
            placeholder="what this session is for"
          />
        </label>
        <label className="field">
          <span className="field-label">
            {role === SessionRole.SUPERVISOR ? "mission (optional)" : "prompt (optional)"}
          </span>
          <textarea
            autoFocus={bucketId !== undefined}
            value={prompt}
            onChange={(e) => setPrompt(e.target.value)}
            rows={6}
            placeholder={role === SessionRole.SUPERVISOR
              ? "what this supervisor should drive — leave blank to brief it in its terminal"
              : "the task to hand the agent — leave blank to start the agent and type in its terminal"}
          />
        </label>
        {role === SessionRole.WORKER && <details className="spawn-advanced"><summary>advanced restrictions</summary><label className="sb-check spawn-items-api"><input type="checkbox" checked={itemsApi} onChange={(e)=>setItemsApi(e.target.checked)} />Items API — let this Worker file and update board items</label></details>}
        {harness.panel}
        {error && <p className="form-error">{error}</p>}
        <div className="modal-actions">
          <button type="button" className="btn" onClick={onClose}>
            cancel
          </button>
          <button type="submit" className="btn btn-primary" disabled={busy || !hasProjects || Boolean(unavailableReason)}>
            {busy ? "spawning…" : "spawn"}
          </button>
        </div>
      </form>
    </div>
  );
}
