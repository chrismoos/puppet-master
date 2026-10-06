import { useHarnessInstall } from "../components/HarnessInstall";
import { InstallCancelled } from "../state/harnessInstall";
import {
  useEffect,
  useLayoutEffect,
  useReducer,
  useRef,
  useState,
  type FormEvent,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import { DirectoryPicker } from "../components/DirectoryPicker";
import { LOCAL_WORKER_ID } from "@puppet-master/client-core/format";
import { AgentKind, PermissionMode, SessionRole } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import type { AppState } from "@puppet-master/client-core/state/reducer";
import { useAppState, useClient } from "../state/hooks";
import { permissionModeLabel, resolvePermissionMode } from "@puppet-master/client-core/state/permission";
import { AGENTS, agentLabel, resolveAgent } from "@puppet-master/client-core/state/agent";
import {
  coveredAgents,
  profileApplies,
  resolveModelProfile,
  selectEndpoint,
} from "@puppet-master/client-core/state/modelProfile";
import { defaultSpawnCwd, initializeSpawnCwd, spawnCwdReducer } from "@puppet-master/client-core/state/spawnCwd";
import { defaultSpawnProjectId, readRememberedSpawnProject } from "../state/supervisorSpawn";
import { orderWorkers, resolveProjectWorkerId, workerUnavailableReason } from "@puppet-master/client-core/state/worker";
import { blocksSpawn } from "../api/projectHost";
import { useProjectHost } from "../api/useProjectHost";
import { submitSpawn } from "../state/spawnSubmit";

/** The bucket whose ＋ opened the popover; role and project are picked in it. */
export type SpawnPopoverTarget = { kind: "bucket"; bucketId: string };

export type SpawnChipKey = "role" | "project" | "host" | "agent";

/** Esc closes an open chip menu before it dismisses the popover. */
export function escapeClosesMenuFirst(chipMenu: SpawnChipKey | null): "menu" | "popover" {
  return chipMenu === null ? "popover" : "menu";
}

/** Everything the user may adjust before spawning. */
export interface SpawnDraft {
  role: SessionRole;
  projectId: string;
  workerId: bigint;
  agentOverride: AgentKind | undefined;
  profileOverride: string;
  permMode: PermissionMode;
  title: string;
  prompt: string;
  itemsApi: boolean;
  cwd: string;
}

/// How close a menu may come to the window edge before it is pulled back.
const CHIP_MENU_MARGIN = 8;

/// Returns how far a menu must move along x to sit inside the window.
///
/// The chips sit inline in a sentence that reflows, so a chip can be
/// anywhere on the line and no fixed edge is right for every one of
/// them: anchoring left sends the menu off the right of a chip near the
/// end, anchoring right sends it off the left of a chip near the start.
/// The menu keeps its natural anchor and is pulled back only as far as
/// it has to be, which is why a menu that already fits does not move.
export function chipMenuShift(
  menu: { left: number; right: number },
  viewportWidth: number,
  margin = CHIP_MENU_MARGIN,
): number {
  // Both edges are considered every time. Pulling the right edge in can
  // push the left one out, so the left is applied second and wins: a menu
  // too wide for the window keeps the options nearest the chip reachable
  // rather than centring the overflow.
  let shift = 0;
  const overflowRight = menu.right - (viewportWidth - margin);
  if (overflowRight > 0) shift = -overflowRight;
  const overflowLeft = margin - (menu.left + shift);
  if (overflowLeft > 0) shift += overflowLeft;
  // Negated zero is still zero but renders as translateX(-0px).
  return shift === 0 ? 0 : shift;
}

export const SPAWN_POPOVER_MARGIN = 12;

/// Calculates the floating viewport coordinates for the spawn popover so that it
/// anchors to the trigger button and floats on top of the layout (including over
/// the agent session to the right), flipping above or clamping when needed.
export function spawnPopoverPosition(
  triggerRect: { top: number; bottom: number; left: number },
  panelRect: { width: number; height: number },
  viewport: { width: number; height: number },
  margin = SPAWN_POPOVER_MARGIN,
): { top: number; left: number } {
  let left = triggerRect.left;
  if (left + panelRect.width > viewport.width - margin) {
    left = Math.max(margin, viewport.width - panelRect.width - margin);
  }
  left = Math.max(margin, left);

  let top = triggerRect.bottom + 4;
  if (top + panelRect.height > viewport.height - margin) {
    const topAbove = triggerRect.top - panelRect.height - 4;
    if (topAbove >= margin) {
      top = topAbove;
    } else {
      top = Math.max(margin, viewport.height - panelRect.height - margin);
    }
  }

  return { top, left };
}

function Chip({
  chipKey,
  label,
  changed,
  open,
  onToggle,
  children,
}: {
  chipKey: SpawnChipKey;
  label: string;
  changed: boolean;
  open: boolean;
  onToggle: () => void;
  children: ReactNode;
}) {
  const menuRef = useRef<HTMLDivElement | null>(null);
  const [shift, setShift] = useState(0);

  // Measured after layout but before paint, because its width depends on
  // its longest option and the chip's position on how the sentence
  // wrapped. A plain effect would let the menu paint once at its
  // unadjusted position and then jump.
  useLayoutEffect(() => {
    if (!open) {
      setShift(0);
      return;
    }
    const menu = menuRef.current;
    if (!menu) return;
    const measure = () => {
      const element = menuRef.current;
      if (!element) return;
      element.style.transform = "";
      const box = element.getBoundingClientRect();
      setShift(chipMenuShift(box, window.innerWidth));
    };
    measure();
    window.addEventListener("resize", measure);
    return () => window.removeEventListener("resize", measure);
  }, [open]);

  return (
    <span className="spawn-chip-wrap">
      <button
        type="button"
        className={`spawn-chip ${changed ? "" : "is-default"}`}
        data-chip={chipKey}
        aria-haspopup="menu"
        aria-expanded={open}
        onClick={onToggle}
      >
        {label}
      </button>
      {open && (
        <div
          ref={menuRef}
          className="popover-menu popover-menu-left spawn-chip-menu"
          role="menu"
          style={shift ? { transform: `translateX(${shift}px)` } : undefined}
        >
          {children}
        </div>
      )}
    </span>
  );
}

function ChipOption({
  label,
  active,
  onPick,
}: {
  label: string;
  active: boolean;
  onPick: () => void;
}) {
  return (
    <button
      type="button"
      role="menuitemradio"
      aria-checked={active}
      className="popover-item popover-radio"
      onClick={onPick}
    >
      <span className="popover-radio-dot" aria-hidden="true">{active ? "●" : "○"}</span>
      {label}
    </button>
  );
}

/** The popover surface, pure props to markup so every state is testable. */
export function SpawnPopoverPanel({
  target,
  state,
  draft,
  defaultProjectId,
  chipMenu,
  expanded,
  busy,
  error,
  installation,
  panelRef,
  style,
  onToggleChipMenu,
  onPickRole,
  onPickProject,
  onPickWorker,
  onPickAgent,
  onSetProfile,
  onSetPermMode,
  onSetTitle,
  onSetPrompt,
  onSetItemsApi,
  onEditCwd,
  onToggleExpanded,
  onDismiss,
  onSubmit,
}: {
  target: SpawnPopoverTarget;
  state: AppState;
  draft: SpawnDraft;
  /** The project the sentence opened with, for the chip's default styling. */
  defaultProjectId: string;
  chipMenu: SpawnChipKey | null;
  expanded: boolean;
  busy: boolean;
  error: string | null;
  installation?: ReactNode;
  panelRef?: React.Ref<HTMLFormElement>;
  style?: React.CSSProperties;
  onToggleChipMenu: (key: SpawnChipKey) => void;
  onPickRole: (role: SessionRole) => void;
  onPickProject: (projectId: string) => void;
  onPickWorker: (workerId: bigint) => void;
  onPickAgent: (agent: AgentKind | undefined) => void;
  onSetProfile: (profileId: string) => void;
  onSetPermMode: (mode: PermissionMode) => void;
  onSetTitle: (title: string) => void;
  onSetPrompt: (prompt: string) => void;
  onSetItemsApi: (enabled: boolean) => void;
  onEditCwd: (cwd: string) => void;
  onToggleExpanded: () => void;
  onDismiss: () => void;
  onSubmit: () => void;
}) {
  const supervisor = draft.role === SessionRole.SUPERVISOR;
  const selectedProject = draft.projectId ? state.projects.get(draft.projectId) : undefined;
  const selectedBucket = selectedProject
    ? state.buckets.get(selectedProject.bucketId.toString())
    : undefined;
  const bucketId = target.bucketId;
  const projects = [...state.projects.values()]
    .filter((project) => project.bucketId.toString() === bucketId)
    .sort((a, b) => a.name.localeCompare(b.name));
  const hasProjects = projects.length > 0;

  const inheritedAgent = resolveAgent(selectedProject, selectedBucket);
  const effectiveAgent = resolveAgent(selectedProject, selectedBucket, draft.agentOverride);
  const inheritedProfile = resolveModelProfile(selectedProject, selectedBucket);
  const effectiveProfile = resolveModelProfile(
    selectedProject,
    selectedBucket,
    draft.profileOverride ? BigInt(draft.profileOverride) : undefined,
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
    draft.permMode,
    selectedProject?.permissionMode ?? PermissionMode.UNSPECIFIED,
    selectedBucket?.permissionMode ?? PermissionMode.UNSPECIFIED,
  );
  const bypassing = effectiveMode === PermissionMode.BYPASS;

  const workers = orderWorkers([...state.workers.values()]).filter(
    (worker) => !selectedProject || selectedProject.allowedWorkerIds.includes(worker.id),
  );
  const selectedWorker = state.workers.get(draft.workerId.toString());
  const projectHost = useProjectHost(
    selectedProject?.id,
    draft.workerId,
    selectedWorker?.online ?? false,
  );
  // A reachable host with an unusable project path is a config problem,
  // not an outage, so its own sentence wins over the generic one.
  const unavailableReason = blocksSpawn(projectHost)
    ? projectHost!.detail
    : workerUnavailableReason(state.workers, draft.workerId);
  const defaultWorkerId = resolveProjectWorkerId(selectedProject, selectedBucket);
  const defaultCwd = defaultSpawnCwd(selectedProject, draft.workerId);

  const hostLabel = selectedWorker
    ? selectedWorker.name
    : draft.workerId === LOCAL_WORKER_ID
      ? "local (disabled)"
      : `worker ${draft.workerId} (unavailable)`;

  const roleChip = (
    <Chip
      chipKey="role"
      label={supervisor ? "supervisor" : "worker"}
      changed={!supervisor}
      open={chipMenu === "role"}
      onToggle={() => onToggleChipMenu("role")}
    >
      <ChipOption
        label="worker"
        active={!supervisor}
        onPick={() => onPickRole(SessionRole.WORKER)}
      />
      <ChipOption
        label="supervisor"
        active={supervisor}
        onPick={() => onPickRole(SessionRole.SUPERVISOR)}
      />
    </Chip>
  );
  const projectChip = (
    <Chip
      chipKey="project"
      label={selectedProject?.name ?? "project"}
      changed={draft.projectId !== defaultProjectId}
      open={chipMenu === "project"}
      onToggle={() => onToggleChipMenu("project")}
    >
      {projects.map((project) => (
        <ChipOption
          key={project.id.toString()}
          label={project.name}
          active={project.id.toString() === draft.projectId}
          onPick={() => onPickProject(project.id.toString())}
        />
      ))}
    </Chip>
  );
  const hostChip = (
    <Chip
      chipKey="host"
      label={hostLabel}
      changed={draft.workerId !== defaultWorkerId}
      open={chipMenu === "host"}
      onToggle={() => onToggleChipMenu("host")}
    >
      {workers.map((worker) => (
        <ChipOption
          key={worker.id.toString()}
          label={`${worker.id === LOCAL_WORKER_ID ? `${worker.name} (local)` : worker.name}${worker.online ? "" : " — offline"}`}
          active={worker.id === draft.workerId}
          onPick={() => onPickWorker(worker.id)}
        />
      ))}
    </Chip>
  );
  const agentChip = (
    <Chip
      chipKey="agent"
      label={agentLabel(effectiveAgent.agent)}
      changed={draft.agentOverride !== undefined}
      open={chipMenu === "agent"}
      onToggle={() => onToggleChipMenu("agent")}
    >
      <ChipOption
        label={`${agentLabel(inheritedAgent.agent)} — ${inheritedAgent.source} default`}
        active={draft.agentOverride === undefined}
        onPick={() => onPickAgent(undefined)}
      />
      {AGENTS.map((agent) => (
        <ChipOption
          key={agent.value}
          label={agent.label}
          active={draft.agentOverride === agent.kind}
          onPick={() => onPickAgent(agent.kind)}
        />
      ))}
    </Chip>
  );

  return (
    <form
      ref={panelRef}
      style={style}
      className="spawn-pop"
      role="dialog"
      aria-label={supervisor
        ? `new supervisor in ${(bucketId ? state.buckets.get(bucketId)?.name : undefined) ?? "bucket"}`
        : `new session in ${selectedProject?.name ?? "project"}`}
      onSubmit={(e: FormEvent) => {
        e.preventDefault();
        if (busy) return;
        onSubmit();
      }}
      onKeyDown={(e) => {
        // Enter spawns from anywhere in the popover except a textarea, a
        // button's own activation, or a field that already consumed it.
        if (e.key !== "Enter" || e.defaultPrevented || chipMenu !== null) return;
        if (e.target instanceof HTMLTextAreaElement || e.target instanceof HTMLButtonElement) return;
        e.preventDefault();
        if (busy) return;
        onSubmit();
      }}
    >
      <button
        type="button"
        className="spawn-pop-close"
        aria-label="dismiss"
        onClick={onDismiss}
      >
        <span aria-hidden="true">✕</span>
      </button>
      <p className="spawn-pop-sentence">
        {roleChip} in {projectChip} on {hostChip} as {agentChip}
      </p>
      {!hasProjects && (
        <p className="muted-line">
          no projects yet — create one under <a href="#/settings/projects">Settings</a> first
        </p>
      )}
      {state.workers.size === 0 && (
        <p className="form-error">no workers registered; local worker is disabled for this daemon</p>
      )}
      {unavailableReason && state.workers.size > 0 && (
        <span className="field-warn">{unavailableReason}</span>
      )}
      {effectiveProfileRecord && !profileEndpoint && profileApplies(effectiveAgent.agent, state.agentDialects) && (
        <span className="field-warn">
          {effectiveProfileRecord.name} has no endpoint {agentLabel(effectiveAgent.agent)} can use — this spawn will be rejected
        </span>
      )}
      {installation}
      {error && <p className="form-error">{error}</p>}
      <div className="spawn-pop-actions">
        <button type="button" className="spawn-pop-fold" onClick={onToggleExpanded} aria-expanded={expanded}>
          {expanded ? "▴ fewer options" : "▾ permissions, profile, directory"}
        </button>
        <button
          type="submit"
          className="btn btn-primary"
          disabled={busy || !hasProjects || Boolean(unavailableReason)}
        >
          {busy ? "spawning…" : "spawn"}
          {!busy && <span className="spawn-pop-kbd" aria-hidden="true"> ⏎</span>}
        </button>
      </div>
      {expanded && (
        <div className="spawn-pop-more">
          <label className="field">
            <span className="field-label">permission mode</span>
            <select
              value={draft.permMode}
              onChange={(e) => onSetPermMode(Number(e.target.value) as PermissionMode)}
              className={bypassing ? "select-danger" : ""}
            >
              <option value={PermissionMode.UNSPECIFIED}>inherit (project default)</option>
              <option value={PermissionMode.DEFAULT}>default</option>
              <option value={PermissionMode.AUTO}>auto</option>
              <option value={PermissionMode.BYPASS}>bypass (dangerous)</option>
            </select>
            {draft.permMode === PermissionMode.UNSPECIFIED && selectedProject && (
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
          {state.modelProfiles.size > 0 && (
            <label className="field">
              <span className="field-label">model profile</span>
              {/* An explicit name keeps profile names, which are user data,
                  out of this control's accessible name. */}
              <select
                aria-label="model profile"
                value={draft.profileOverride}
                onChange={(e) => onSetProfile(e.target.value)}
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
            <span className="field-label">working directory</span>
            <DirectoryPicker
              value={draft.cwd}
              onChange={onEditCwd}
              placeholder={defaultCwd || "/home/you/src/project"}
              workerId={draft.workerId}
            />
          </label>
          <label className="field">
            <span className="field-label">title (optional)</span>
            <input
              value={draft.title}
              onChange={(e) => onSetTitle(e.target.value)}
              placeholder="what this session is for"
            />
          </label>
          <label className="field">
            <span className="field-label">starting prompt (optional)</span>
            <textarea
              value={draft.prompt}
              onChange={(e) => onSetPrompt(e.target.value)}
              rows={3}
              placeholder="rarely needed — you brief the agent in its terminal"
            />
          </label>
          {!supervisor && (
            <label className="sb-check spawn-items-api">
              <input
                type="checkbox"
                checked={draft.itemsApi}
                onChange={(e) => onSetItemsApi(e.target.checked)}
              />
              Items API — let this Worker file and update board items
            </label>
          )}
        </div>
      )}
    </form>
  );
}

function SpawnPopoverForm({
  triggerRef,
  target,
  onClose,
  onDismiss,
  onCreated,
}: {
  triggerRef: React.RefObject<HTMLButtonElement | null>;
  target: SpawnPopoverTarget;
  onClose: () => void;
  /** Close and return focus to the ＋ that opened the popover. */
  onDismiss: () => void;
  onCreated: (id: string) => void;
}) {
  const client = useClient();
  const state = useAppState();
  const [role, setRole] = useState(SessionRole.SUPERVISOR);
  const supervisor = role === SessionRole.SUPERVISOR;
  const workerForProject = (id: string): bigint => {
    const project = state.projects.get(id);
    const bucket = project ? state.buckets.get(project.bucketId.toString()) : undefined;
    return resolveProjectWorkerId(project, bucket);
  };
  const rememberedProjectId = (forRole: SessionRole): string =>
    defaultSpawnProjectId(
      target.bucketId,
      state.projects.values(),
      readRememberedSpawnProject(target.bucketId, forRole),
    );
  const [defaultProjectId] = useState(() => rememberedProjectId(SessionRole.SUPERVISOR));
  const [projectId, setProjectId] = useState(defaultProjectId);
  const [workerId, setWorkerId] = useState<bigint>(() =>
    projectId ? workerForProject(projectId) : LOCAL_WORKER_ID,
  );
  const [agentOverride, setAgentOverride] = useState<AgentKind | undefined>();
  const [profileOverride, setProfileOverride] = useState("");
  const [permMode, setPermMode] = useState<PermissionMode>(PermissionMode.UNSPECIFIED);
  const [title, setTitle] = useState("");
  const [prompt, setPrompt] = useState("");
  const [itemsApi, setItemsApi] = useState(true);
  const [cwdState, dispatchCwd] = useReducer(
    spawnCwdReducer,
    defaultSpawnCwd(projectId ? state.projects.get(projectId) : undefined, workerId),
    initializeSpawnCwd,
  );
  const [chipMenu, setChipMenu] = useState<SpawnChipKey | null>(null);
  const [expanded, setExpanded] = useState(false);
  const [busy, setBusy] = useState(false);
  const harness = useHarnessInstall();
  const [error, setError] = useState<string | null>(null);
  const formRef = useRef<HTMLFormElement>(null);
  const chipMenuRef = useRef(chipMenu);
  chipMenuRef.current = chipMenu;
  const dismissRef = useRef(onDismiss);
  dismissRef.current = onDismiss;

  const [position, setPosition] = useState<{ top: number; left: number } | null>(() => {
    if (typeof window === "undefined" || !triggerRef.current) return null;
    return spawnPopoverPosition(
      triggerRef.current.getBoundingClientRect(),
      { width: 440, height: 100 },
      { width: window.innerWidth, height: window.innerHeight },
    );
  });

  useLayoutEffect(() => {
    const updatePosition = () => {
      const trigger = triggerRef.current;
      const form = formRef.current;
      if (!trigger || !form) return;
      const triggerRect = trigger.getBoundingClientRect();
      const formRect = form.getBoundingClientRect();
      const viewport = { width: window.innerWidth, height: window.innerHeight };

      if (triggerRect.bottom < 0 || triggerRect.top > viewport.height) {
        onClose();
        return;
      }

      setPosition(spawnPopoverPosition(triggerRect, formRect, viewport));
    };

    updatePosition();
    window.addEventListener("resize", updatePosition);
    window.addEventListener("scroll", updatePosition, true);
    return () => {
      window.removeEventListener("resize", updatePosition);
      window.removeEventListener("scroll", updatePosition, true);
    };
  }, [expanded, onClose, triggerRef]);

  const selectedProject = projectId ? state.projects.get(projectId) : undefined;
  const projectHost = useProjectHost(
    selectedProject?.id,
    workerId,
    state.workers.get(workerId.toString())?.online ?? false,
  );
  const unavailableReason = blocksSpawn(projectHost)
    ? projectHost!.detail
    : workerUnavailableReason(state.workers, workerId);
  const defaultCwd = defaultSpawnCwd(selectedProject, workerId);
  useEffect(() => {
    dispatchCwd({ type: "default-changed", value: defaultCwd });
  }, [defaultCwd]);

  // Focus lands on the spawn button, so ＋ then Enter spawns with defaults.
  useEffect(() => {
    formRef.current?.querySelector<HTMLButtonElement>('button[type="submit"]')?.focus();
  }, []);

  useEffect(() => {
    // Capture Escape before terminal widgets can consume it, matching Popover.
    const onKey = (e: KeyboardEvent) => {
      if (e.key !== "Escape") return;
      e.preventDefault();
      e.stopPropagation();
      if (escapeClosesMenuFirst(chipMenuRef.current) === "menu") setChipMenu(null);
      else dismissRef.current();
    };
    const onDown = (e: MouseEvent) => {
      if (chipMenuRef.current === null) return;
      if (!(e.target instanceof Element) || !e.target.closest(".spawn-chip-wrap")) setChipMenu(null);
    };
    document.addEventListener("keydown", onKey, true);
    document.addEventListener("mousedown", onDown);
    return () => {
      document.removeEventListener("keydown", onKey, true);
      document.removeEventListener("mousedown", onDown);
    };
  }, []);

  const submit = () => {
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
    // Untouched means "use the effective project default", exactly as the
    // spawn dialog distinguishes a displayed default from an override.
    harness.prepare({
      projectId,
      agent: agentOverride,
      title,
      prompt,
      cwdOverride: cwdState.touched ? cwdState.value.trim() : "",
      permissionMode: permMode,
      workerId,
      itemsApi: supervisor ? true : itemsApi,
      role,
      modelProfileId: profileOverride ? BigInt(profileOverride) : undefined,
      spawnBucketId: target.bucketId,
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
    <SpawnPopoverPanel
      panelRef={formRef}
      style={position ? { top: `${position.top}px`, left: `${position.left}px` } : undefined}
      target={target}
      state={state}
      draft={{
        role,
        projectId,
        workerId,
        agentOverride,
        profileOverride,
        permMode,
        title,
        prompt,
        itemsApi,
        cwd: cwdState.value,
      }}
      defaultProjectId={defaultProjectId}
      chipMenu={chipMenu}
      expanded={expanded}
      busy={busy}
      error={error}
      installation={harness.panel}
      onToggleChipMenu={(key) => setChipMenu((cur) => (cur === key ? null : key))}
      onPickRole={(nextRole) => {
        setRole(nextRole);
        const nextProjectId = rememberedProjectId(nextRole);
        if (nextProjectId !== projectId) {
          const nextWorkerId = workerForProject(nextProjectId);
          setProjectId(nextProjectId);
          setWorkerId(nextWorkerId);
          dispatchCwd({
            type: "selection-changed",
            value: defaultSpawnCwd(state.projects.get(nextProjectId), nextWorkerId),
          });
        }
        setChipMenu(null);
      }}
      onPickProject={(nextProjectId) => {
        const nextWorkerId = workerForProject(nextProjectId);
        setProjectId(nextProjectId);
        setWorkerId(nextWorkerId);
        dispatchCwd({
          type: "selection-changed",
          value: defaultSpawnCwd(state.projects.get(nextProjectId), nextWorkerId),
        });
        setChipMenu(null);
      }}
      onPickWorker={(nextWorkerId) => {
        setWorkerId(nextWorkerId);
        dispatchCwd({
          type: "selection-changed",
          value: defaultSpawnCwd(selectedProject, nextWorkerId),
        });
        setChipMenu(null);
      }}
      onPickAgent={(agent) => {
        setAgentOverride(agent);
        setChipMenu(null);
      }}
      onSetProfile={setProfileOverride}
      onSetPermMode={setPermMode}
      onSetTitle={setTitle}
      onSetPrompt={setPrompt}
      onSetItemsApi={setItemsApi}
      onEditCwd={(value) => dispatchCwd({ type: "edit", value })}
      onToggleExpanded={() => setExpanded((cur) => !cur)}
      onDismiss={onDismiss}
      onSubmit={submit}
    />
  );
}

/**
 * The ＋ button with its one-line spawn popover. The parent owns `open` so
 * one popover (or row menu) is open at a time, like the sidebar's Popover
 * menus. Spawn-from-item and pm:spawn links keep the full SpawnDialog.
 */
export function SpawnPopover({
  open,
  onToggle,
  onClose,
  onCreated,
  triggerTitle,
  target,
}: {
  open: boolean;
  onToggle: () => void;
  onClose: () => void;
  onCreated: (id: string) => void;
  triggerTitle: string;
  target: SpawnPopoverTarget;
}) {
  const wrapRef = useRef<HTMLDivElement>(null);
  const triggerRef = useRef<HTMLButtonElement>(null);
  const closeRef = useRef(onClose);
  closeRef.current = onClose;

  useEffect(() => {
    if (!open) return;
    const onDown = (e: MouseEvent) => {
      const target = e.target as Node;
      if (wrapRef.current && wrapRef.current.contains(target)) return;
      if (e.target instanceof Element && e.target.closest(".spawn-pop")) return;
      closeRef.current();
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [open]);

  const dismiss = () => {
    onClose();
    requestAnimationFrame(() => triggerRef.current?.focus());
  };

  const form = open ? (
    <SpawnPopoverForm
      triggerRef={triggerRef}
      target={target}
      onClose={onClose}
      onDismiss={dismiss}
      onCreated={onCreated}
    />
  ) : null;

  return (
    <div className="popover spawn-popover" ref={wrapRef}>
      <button
        ref={triggerRef}
        type="button"
        className="sb-icon-btn"
        aria-haspopup="dialog"
        aria-expanded={open}
        title={triggerTitle}
        onClick={onToggle}
      >
        <span aria-hidden="true">＋</span>
      </button>
      {form && (typeof document === "undefined" ? form : createPortal(form, document.body))}
    </div>
  );
}
