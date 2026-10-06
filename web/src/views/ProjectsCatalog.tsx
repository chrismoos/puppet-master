import { useEffect, useMemo, useRef, useState, type FormEvent, type ReactNode } from "react";
import { DirectoryPicker } from "../components/DirectoryPicker";
import { LOCAL_WORKER_ID } from "@puppet-master/client-core/format";
import { PermissionMode } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import type { AgentDialects, Bucket, ModelProfile, Project, ProjectPath, Worker } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { navigate, type SettingsCatalog } from "../router";
import { SettingsPageHead } from "./settingsParts";
import { useAppState, useClient } from "../state/hooks";
import { orderWorkers, resolveProjectWorkerId } from "@puppet-master/client-core/state/worker";
import { setBucketWorker, setDefaultBucket, setProjectWorker, setProjectWorkerPath } from "../api/workers";
import { agentFromValue, agentLabel, agentValue, resolveAgent } from "@puppet-master/client-core/state/agent";
import { AgentSelect } from "../components/AgentSelect";
import { coveredAgents } from "@puppet-master/client-core/state/modelProfile";
import { permissionModeLabel, resolvePermissionMode } from "@puppet-master/client-core/state/permission";

type CreateKind = "bucket" | "project" | null;

const PERMISSION_CHOICES = [PermissionMode.DEFAULT, PermissionMode.AUTO, PermissionMode.BYPASS];

export function permissionValue(mode: PermissionMode): string {
  return mode === PermissionMode.UNSPECIFIED ? "" : String(mode);
}

export function permissionFromValue(value: string): PermissionMode {
  return value ? (Number(value) as PermissionMode) : PermissionMode.UNSPECIFIED;
}

export function catalogModeForRoute(
  catalog: SettingsCatalog | undefined,
  selectedBucketId?: string,
  selectedProjectId?: string,
): SettingsCatalog {
  if (selectedProjectId) return "projects";
  if (selectedBucketId) return "buckets";
  return catalog ?? "projects";
}

function workerName(worker: Worker | undefined, id: bigint): string {
  if (worker) return worker.id === LOCAL_WORKER_ID ? `${worker.name} (local)` : worker.name;
  return id === LOCAL_WORKER_ID ? "local (disabled)" : `worker ${id} (unavailable)`;
}

/**
 * The project's Default Worker once its allowed Workers become `allowed`.
 * Unchecking the Worker a project points at — its own override, or the
 * bucket default it inherits — would otherwise leave a selection the
 * server rejects, so it moves to a Worker the project still allows.
 * Empty means it keeps inheriting the bucket.
 */
export function nextProjectDefaultHost(
  allowed: readonly string[],
  current: string,
  bucketDefault: string,
): string {
  if (current) return allowed.includes(current) ? current : allowed[0];
  return allowed.includes(bucketDefault) ? "" : allowed[0];
}

/**
 * The per-worker path writes needed to reconcile the drawer's edited
 * worker paths with the stored rows: a trimmed non-empty edit that
 * differs from the stored row sets, an emptied edit with a stored row
 * clears, and workers outside the allowed set are never written.
 */
export function workerPathUpdates(
  stored: readonly Pick<ProjectPath, "workerId" | "path">[],
  edited: Readonly<Record<string, string>>,
  allowed: readonly string[],
): { workerId: bigint; path: string | null }[] {
  const updates: { workerId: bigint; path: string | null }[] = [];
  for (const id of allowed) {
    const value = (edited[id] ?? "").trim();
    const row = stored.find((mapping) => mapping.workerId.toString() === id);
    if (value && value !== row?.path) updates.push({ workerId: BigInt(id), path: value });
    if (!value && row) updates.push({ workerId: BigInt(id), path: null });
  }
  return updates;
}

function sortedBuckets(values: Iterable<Bucket>): Bucket[] {
  return [...values].sort((a, b) => a.position - b.position || a.name.localeCompare(b.name));
}

function sortedProjects(values: Iterable<Project>): Project[] {
  return [...values].sort((a, b) => a.name.localeCompare(b.name));
}

function sortedProfiles(values: Iterable<ModelProfile>): ModelProfile[] {
  return [...values].sort((a, b) => a.name.localeCompare(b.name));
}

function profileName(profiles: ReadonlyMap<string, ModelProfile>, id: bigint | undefined): string {
  if (id === undefined) return "Agent account";
  return profiles.get(id.toString())?.name ?? `profile ${id} (unavailable)`;
}

export function ProjectsCatalog({
  catalog,
  selectedBucketId,
  selectedProjectId,
  preselectBucket,
  onPreselectConsumed,
}: {
  catalog?: SettingsCatalog;
  selectedBucketId?: string;
  selectedProjectId?: string;
  preselectBucket?: string | null;
  onPreselectConsumed?: () => void;
}) {
  const state = useAppState();
  const client = useClient();
  const mode = catalogModeForRoute(catalog, selectedBucketId, selectedProjectId);
  const buckets = sortedBuckets(state.buckets.values());
  const projects = sortedProjects(state.projects.values());
  const workers = orderWorkers([...state.workers.values()]);
  const modelProfiles = sortedProfiles(state.modelProfiles.values());
  const selectedBucket = selectedBucketId ? state.buckets.get(selectedBucketId) : undefined;
  const selectedProject = selectedProjectId ? state.projects.get(selectedProjectId) : undefined;
  const selectedProjectBucket = selectedProject
    ? state.buckets.get(selectedProject.bucketId.toString())
    : undefined;
  const selectedRowRef = useRef<HTMLTableRowElement | null>(null);
  const detailHeadingRef = useRef<HTMLHeadingElement | null>(null);

  const [query, setQuery] = useState("");
  const [creating, setCreating] = useState<CreateKind>(null);
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const [confirmDelete, setConfirmDelete] = useState(false);

  const [bucketDefaultWorker, setBucketDefaultWorker] = useState("");
  const [bucketAllowed, setBucketAllowed] = useState<string[]>([]);
  const [bucketReplacement, setBucketReplacement] = useState("");
  const [bucketAgent, setBucketAgent] = useState("");
  const [bucketProfile, setBucketProfile] = useState("");
  const [bucketPermission, setBucketPermission] = useState("");
  const [projectPath, setProjectPath] = useState("");
  const [projectWorker, setProjectWorkerValue] = useState("");
  const [projectAllowed, setProjectAllowed] = useState<string[]>([]);
  const [projectHostPaths, setProjectHostPaths] = useState<Record<string, string>>({});
  const [projectAgent, setProjectAgent] = useState("");
  const [projectProfile, setProjectProfile] = useState("");
  const [projectPermission, setProjectPermission] = useState("");

  const [newBucketName, setNewBucketName] = useState("");
  const [newBucketDefault, setNewBucketDefault] = useState("");
  const [newBucketAllowed, setNewBucketAllowed] = useState<string[]>([]);
  const [newBucketIsDefault, setNewBucketIsDefault] = useState(false);
  const [newBucketAgent, setNewBucketAgent] = useState("claude");
  const [newProjectBucket, setNewProjectBucket] = useState("");
  const [newProjectName, setNewProjectName] = useState("");
  const [newProjectPath, setNewProjectPath] = useState("");
  const [newProjectWorker, setNewProjectWorker] = useState("");
  const [newProjectAllowed, setNewProjectAllowed] = useState<string[]>([]);
  const [newProjectAgent, setNewProjectAgent] = useState("");

  useEffect(() => {
    if (!selectedBucket) return;
    setBucketDefaultWorker(selectedBucket.defaultWorkerId.toString());
    setBucketAllowed(selectedBucket.allowedWorkerIds.map(String));
    setBucketReplacement("");
    setBucketAgent(agentValue(selectedBucket.defaultAgent));
    setBucketProfile(selectedBucket.modelProfileId?.toString() ?? "");
    setBucketPermission(permissionValue(selectedBucket.permissionMode));
    setConfirmDelete(false);
    setCreating(null);
  }, [selectedBucket]);

  useEffect(() => {
    if (!selectedProject) return;
    setProjectPath(selectedProject.path);
    setProjectWorkerValue(selectedProject.workerId?.toString() ?? "");
    setProjectAllowed(selectedProject.allowedWorkerIds.map(String));
    setProjectHostPaths(
      Object.fromEntries(selectedProject.workerPaths.map((mapping) => [mapping.workerId.toString(), mapping.path])),
    );
    setProjectAgent(agentValue(selectedProject.defaultAgent));
    setProjectProfile(selectedProject.modelProfileId?.toString() ?? "");
    setProjectPermission(permissionValue(selectedProject.permissionMode));
    setConfirmDelete(false);
    setCreating(null);
  }, [selectedProject]);

  useEffect(() => {
    if (!selectedBucket && !selectedProject) return;
    selectedRowRef.current?.scrollIntoView({ block: "center" });
    selectedRowRef.current?.focus({ preventScroll: true });
  }, [selectedBucket, selectedProject]);

  useEffect(() => {
    if (!preselectBucket) return;
    const bucket = state.buckets.get(preselectBucket);
    if (bucket) {
      setNewProjectBucket(preselectBucket);
      setNewProjectWorker("");
      setNewProjectAllowed(bucket.allowedWorkerIds.map(String));
      setCreating("project");
    }
    onPreselectConsumed?.();
  }, [preselectBucket, onPreselectConsumed, state.buckets]);

  useEffect(() => {
    if (newBucketAllowed.length || workers.length === 0) return;
    const first = workers[0].id.toString();
    setNewBucketAllowed([first]);
    setNewBucketDefault(first);
  }, [newBucketAllowed.length, workers]);

  useEffect(() => {
    if (creating) detailHeadingRef.current?.focus({ preventScroll: true });
  }, [creating]);

  const run = async (action: () => Promise<unknown>, after?: () => void) => {
    setError(null);
    setBusy(true);
    try {
      await action();
      after?.();
    } catch (reason) {
      setError(reason instanceof Error ? reason.message : String(reason));
    } finally {
      setBusy(false);
    }
  };

  const openMode = (next: SettingsCatalog) => {
    setCreating(null);
    setError(null);
    navigate(`/settings/projects?catalog=${next}`);
  };

  const visibleBuckets = useMemo(() => {
    const needle = query.trim().toLocaleLowerCase();
    return needle ? buckets.filter((bucket) => bucket.name.toLocaleLowerCase().includes(needle)) : buckets;
  }, [buckets, query]);
  const visibleProjects = useMemo(() => {
    const needle = query.trim().toLocaleLowerCase();
    if (!needle) return projects;
    return projects.filter((project) => {
      const bucket = state.buckets.get(project.bucketId.toString());
      return `${project.name} ${project.path} ${bucket?.name ?? ""}`.toLocaleLowerCase().includes(needle);
    });
  }, [projects, query, state.buckets]);

  const beginCreate = () => {
    setError(null);
    setConfirmDelete(false);
    if (mode === "buckets") {
      setCreating("bucket");
    } else {
      const bucket = buckets[0];
      if (bucket && !newProjectBucket) {
        setNewProjectBucket(bucket.id.toString());
        setNewProjectAllowed(bucket.allowedWorkerIds.map(String));
      }
      setCreating("project");
    }
  };

  const createBucket = (event: FormEvent) => {
    event.preventDefault();
    if (!newBucketDefault || newBucketAllowed.length === 0) {
      setError("select at least one Worker");
      return;
    }
    void run(
      async () => {
        const outcome = await client.createBucket(
          newBucketName.trim(),
          newBucketAllowed.map(BigInt),
          BigInt(newBucketDefault),
          newBucketIsDefault,
        );
        if (outcome.createdId !== undefined) {
          await client.setBucketDefaultAgent(outcome.createdId, agentFromValue(newBucketAgent));
        }
      },
      () => {
        setNewBucketName("");
        setNewBucketIsDefault(false);
        setCreating(null);
      },
    );
  };

  const createProject = (event: FormEvent) => {
    event.preventDefault();
    if (!newProjectBucket || newProjectAllowed.length === 0) {
      setError("select a bucket with at least one Worker");
      return;
    }
    void run(
      async () => {
        const outcome = await client.createProject(
          BigInt(newProjectBucket),
          newProjectName.trim(),
          newProjectPath.trim(),
          newProjectWorker ? BigInt(newProjectWorker) : undefined,
          newProjectAllowed.map(BigInt),
        );
        if (outcome.createdId !== undefined) {
          await client.setProjectDefaultAgent(outcome.createdId, agentFromValue(newProjectAgent));
        }
      },
      () => {
        setNewProjectName("");
        setNewProjectPath("");
        setCreating(null);
      },
    );
  };

  const bucketDroppedHosts = selectedBucket
    ? selectedBucket.allowedWorkerIds.map(String).filter((id) => !bucketAllowed.includes(id))
    : [];
  const bucketDroppedProjects = projects.filter(
    (project) =>
      selectedBucket !== undefined &&
      project.bucketId === selectedBucket.id &&
      (bucketDroppedHosts.includes(project.workerId?.toString() ?? "") ||
        project.allowedWorkerIds.some((id) => bucketDroppedHosts.includes(id.toString()))),
  );

  const saveBucket = (event: FormEvent) => {
    event.preventDefault();
    if (!selectedBucket || !bucketDefaultWorker || bucketAllowed.length === 0) return;
    void run(
      async () => {
        await setBucketWorker(
          selectedBucket.id,
          BigInt(bucketDefaultWorker),
          bucketAllowed.map(BigInt),
          bucketReplacement ? BigInt(bucketReplacement) : undefined,
        );
        await client.setBucketDefaultAgent(selectedBucket.id, agentFromValue(bucketAgent));
        await client.setBucketModelProfile(
          selectedBucket.id,
          bucketProfile ? BigInt(bucketProfile) : undefined,
        );
        await client.setBucketPermissionMode(
          selectedBucket.id,
          permissionFromValue(bucketPermission),
        );
      },
      () => navigate("/settings/projects?catalog=buckets"),
    );
  };

  const saveProject = (event: FormEvent) => {
    event.preventDefault();
    if (!selectedProject || projectAllowed.length === 0) return;
    void run(
      async () => {
        await client.updateProject(selectedProject.id, projectPath.trim());
        await setProjectWorker(
          selectedProject.id,
          projectWorker ? BigInt(projectWorker) : null,
          projectAllowed.map(BigInt),
        );
        for (const update of workerPathUpdates(selectedProject.workerPaths, projectHostPaths, projectAllowed)) {
          await setProjectWorkerPath(selectedProject.id, update.workerId, update.path);
        }
        await client.setProjectDefaultAgent(selectedProject.id, agentFromValue(projectAgent));
        await client.setProjectModelProfile(
          selectedProject.id,
          projectProfile ? BigInt(projectProfile) : undefined,
        );
        await client.setProjectPermissionMode(
          selectedProject.id,
          permissionFromValue(projectPermission),
        );
      },
      () => navigate("/settings/projects?catalog=projects"),
    );
  };

  const bucketWorkerRows = (allowed: string[], onChange: (next: string[]) => void) => {
    const ids = new Set([...workers.map((worker) => worker.id.toString()), ...allowed]);
    return [...ids].map((id) => {
      const checked = allowed.includes(id);
      const worker = state.workers.get(id);
      return (
        <label className="catalog-check" key={id}>
          <input
            type="checkbox"
            checked={checked}
            disabled={!worker}
            onChange={() => onChange(checked ? allowed.filter((value) => value !== id) : [...allowed, id])}
          />
          <span>{workerName(worker, BigInt(id))}</span>
          <small>{worker ? (worker.online ? "online" : "offline") : "unavailable"}</small>
        </label>
      );
    });
  };

  const projectBucketForCreate = newProjectBucket
    ? state.buckets.get(newProjectBucket)
    : undefined;
  const projectCreateEffectiveWorker = resolveProjectWorkerId(
    { workerId: newProjectWorker ? BigInt(newProjectWorker) : undefined },
    projectBucketForCreate,
  );
  const projectEditEffectiveWorker = selectedProject
    ? resolveProjectWorkerId(
        { workerId: projectWorker ? BigInt(projectWorker) : undefined },
        selectedProjectBucket,
      )
    : LOCAL_WORKER_ID;

  const hasDrawer = Boolean(creating || selectedBucket || selectedProject);

  return (
    <section className={`catalogs ${hasDrawer ? "has-drawer" : ""}`} aria-labelledby="settings-page-title">
      <div className="catalogs-main">
        <SettingsPageHead
          title="Projects"
          description="Buckets group projects and carry the defaults each project inherits."
          actions={
            <button type="button" className="btn btn-primary btn-lg" onClick={beginCreate} disabled={mode === "projects" && buckets.length === 0}>
              New {mode === "buckets" ? "bucket" : "project"}
            </button>
          }
        />

        <div className="catalog-tabs" role="tablist" aria-label="Project management catalogs">
          <button type="button" role="tab" aria-selected={mode === "buckets"} onClick={() => openMode("buckets")}>Buckets <b>{buckets.length}</b></button>
          <button type="button" role="tab" aria-selected={mode === "projects"} onClick={() => openMode("projects")}>Projects <b>{projects.length}</b></button>
        </div>

        <div className="catalog-toolbar">
          <label className="catalog-search">
            <span className="visually-hidden">Search {mode}</span>
            <span aria-hidden="true">⌕</span>
            <input value={query} onChange={(event) => setQuery(event.target.value)} placeholder={`Search ${mode}`} />
          </label>
        </div>

        {error && !hasDrawer && <CatalogError message={error} onDismiss={() => setError(null)} />}
        {!state.hydrated ? (
          <CatalogState kind="loading" title={`Loading ${mode}…`} body="Navigation remains available while Puppet Master reconnects." />
        ) : mode === "buckets" ? (
          visibleBuckets.length === 0 ? (
            <CatalogState
              kind="empty"
              title={query ? "No matching buckets" : "No buckets yet"}
              body={query ? "Try a different search." : "Create a bucket to establish project and Worker defaults."}
              action={query ? undefined : <button className="btn btn-primary" onClick={beginCreate}>Create bucket</button>}
            />
          ) : (
            <div className="catalog-table-wrap">
              <table className="catalog-table">
                <thead><tr><th>Bucket</th><th>Projects</th><th>Default Agent</th><th>Model Profile</th><th>Default Worker</th><th>Allowed</th><th><span className="visually-hidden">Open</span></th></tr></thead>
                <tbody>{visibleBuckets.map((bucket) => {
                  const selected = selectedBucket?.id === bucket.id;
                  const count = projects.filter((project) => project.bucketId === bucket.id).length;
                  return (
                    <tr
                      key={bucket.id.toString()}
                      className={`manage-bucket ${selected ? "is-selected" : ""}`}
                      data-bucket-id={bucket.id.toString()}
                      tabIndex={selected ? -1 : undefined}
                      ref={selected ? (node) => { selectedRowRef.current = node; } : undefined}
                    >
                      <td data-label="Bucket"><button className="catalog-name" onClick={() => navigate(`/settings/projects?bucket=${bucket.id}`)}><b>{bucket.name}</b><small>{bucket.isDefault ? "Default bucket" : "Bucket"}</small></button></td>
                      <td data-label="Projects">{count}</td>
                      <td data-label="Default Agent">{agentLabel(resolveAgent(undefined, bucket).agent)}</td>
                      <td data-label="Model Profile">{profileName(state.modelProfiles, bucket.modelProfileId)}</td>
                      <td data-label="Default Worker">{workerName(state.workers.get(bucket.defaultWorkerId.toString()), bucket.defaultWorkerId)}</td>
                      <td data-label="Allowed"><span className="catalog-count">{bucket.allowedWorkerIds.length} Worker{bucket.allowedWorkerIds.length === 1 ? "" : "s"}</span></td>
                      <td><button className="catalog-open" aria-label={`Edit bucket ${bucket.name}`} onClick={() => navigate(`/settings/projects?bucket=${bucket.id}`)}>›</button></td>
                    </tr>
                  );
                })}</tbody>
              </table>
            </div>
          )
        ) : visibleProjects.length === 0 ? (
          <CatalogState
            kind="empty"
            title={query ? "No matching projects" : "No projects yet"}
            body={query ? "Try a different search." : "Create a project inside an existing bucket."}
            action={query || buckets.length === 0 ? undefined : <button className="btn btn-primary" onClick={beginCreate}>Create project</button>}
          />
        ) : (
          <div className="catalog-table-wrap">
            <table className="catalog-table">
              <thead><tr><th>Project</th><th>Bucket</th><th>Default Agent</th><th>Model Profile</th><th>Default Worker</th><th>Allowed</th><th><span className="visually-hidden">Open</span></th></tr></thead>
              <tbody>{visibleProjects.map((project) => {
                const bucket = state.buckets.get(project.bucketId.toString());
                const effective = resolveProjectWorkerId(project, bucket);
                const selected = selectedProject?.id === project.id;
                return (
                  <tr
                    key={project.id.toString()}
                    className={`manage-project ${selected ? "is-selected" : ""}`}
                    data-project-id={project.id.toString()}
                    data-bucket-id={project.bucketId.toString()}
                    tabIndex={selected ? -1 : undefined}
                    ref={selected ? (node) => { selectedRowRef.current = node; } : undefined}
                  >
                    <td data-label="Project"><button className="catalog-name" onClick={() => navigate(`/settings/projects?bucket=${project.bucketId}&project=${project.id}`)}><b>{project.name}</b><small className="project-path">{project.path}</small></button></td>
                    <td data-label="Bucket"><span className="catalog-bucket-pill">{bucket?.name ?? "Unavailable"}</span></td>
                    <td data-label="Default Agent">{project.defaultAgent === undefined && <span className="catalog-inherit">↳ </span>}{agentLabel(resolveAgent(project, bucket).agent)}</td>
                    <td data-label="Model Profile">{project.modelProfileId === undefined && <span className="catalog-inherit">↳ </span>}{profileName(state.modelProfiles, project.modelProfileId ?? bucket?.modelProfileId)}</td>
                    <td data-label="Default Worker">{project.workerId === undefined && <span className="catalog-inherit">↳ </span>}{workerName(state.workers.get(effective.toString()), effective)}</td>
                    <td data-label="Allowed"><span className="catalog-count">{project.allowedWorkerIds.length} Worker{project.allowedWorkerIds.length === 1 ? "" : "s"}</span></td>
                    <td><button className="catalog-open" aria-label={`Edit project ${project.name}`} onClick={() => navigate(`/settings/projects?bucket=${project.bucketId}&project=${project.id}`)}>›</button></td>
                  </tr>
                );
              })}</tbody>
            </table>
          </div>
        )}
      </div>

      {hasDrawer && (
        <aside className="catalog-drawer" aria-label={creating ? `New ${creating}` : selectedProject ? `Edit project ${selectedProject.name}` : `Edit bucket ${selectedBucket?.name}`}>
          <button type="button" className="catalog-drawer-close" aria-label="Close editor" onClick={() => { setCreating(null); navigate(`/settings/projects?catalog=${mode}`); }}>×</button>
          {error && <CatalogError message={error} onDismiss={() => setError(null)} />}
          {creating === "bucket" && (
            <form onSubmit={createBucket}>
              <span className="catalogs-overline bucket-accent">New bucket</span>
              <h3 ref={detailHeadingRef} tabIndex={-1}>Create bucket</h3>
              <CatalogField label="Name"><input value={newBucketName} onChange={(event) => setNewBucketName(event.target.value)} required autoFocus /></CatalogField>
              <fieldset className="catalog-fieldset"><legend>Workers projects may use</legend>{bucketWorkerRows(newBucketAllowed, (next) => { setNewBucketAllowed(next); if (!next.includes(newBucketDefault)) setNewBucketDefault(next[0] ?? ""); })}</fieldset>
              <CatalogField label="Default Worker"><select value={newBucketDefault} onChange={(event) => setNewBucketDefault(event.target.value)} required><option value="" disabled>Pick a Worker…</option>{workers.filter((worker) => newBucketAllowed.includes(worker.id.toString())).map((worker) => <option key={worker.id.toString()} value={worker.id.toString()}>{workerName(worker, worker.id)}</option>)}</select></CatalogField>
              <CatalogField label="Default Agent"><AgentSelect value={newBucketAgent} onChange={setNewBucketAgent} inheritLabel="System default (Claude)" /></CatalogField>
              {buckets.length > 0 && <label className="catalog-check"><input type="checkbox" checked={newBucketIsDefault} onChange={(event) => setNewBucketIsDefault(event.target.checked)} /><span>Make this the default bucket</span></label>}
              <DrawerActions busy={busy} onCancel={() => setCreating(null)} submit="Create bucket" />
            </form>
          )}
          {creating === "project" && (
            <form onSubmit={createProject}>
              <span className="catalogs-overline">New project</span>
              <h3 ref={detailHeadingRef} tabIndex={-1}>Create project</h3>
              <CatalogField label="Bucket"><select value={newProjectBucket} onChange={(event) => { const id = event.target.value; const bucket = state.buckets.get(id); setNewProjectBucket(id); setNewProjectWorker(""); setNewProjectAllowed(bucket?.allowedWorkerIds.map(String) ?? []); }} required>{buckets.map((bucket) => <option key={bucket.id.toString()} value={bucket.id.toString()}>{bucket.name}</option>)}</select></CatalogField>
              <CatalogField label="Name"><input value={newProjectName} onChange={(event) => setNewProjectName(event.target.value)} required /></CatalogField>
              <CatalogField label="Absolute path"><DirectoryPicker value={newProjectPath} onChange={setNewProjectPath} workerId={projectCreateEffectiveWorker} placeholder={state.workers.get(projectCreateEffectiveWorker.toString())?.defaultProjectRoot || "/home/you/src/project"} /></CatalogField>
              <fieldset className="catalog-fieldset"><legend>Allowed Workers</legend>{bucketWorkerRows(newProjectAllowed, setNewProjectAllowed)}</fieldset>
              <CatalogField label="Default Worker"><select value={newProjectWorker} onChange={(event) => setNewProjectWorker(event.target.value)}><option value="">Inherit bucket</option>{workers.filter((worker) => newProjectAllowed.includes(worker.id.toString())).map((worker) => <option key={worker.id.toString()} value={worker.id.toString()}>{workerName(worker, worker.id)}</option>)}</select></CatalogField>
              <CatalogField label="Default Agent"><AgentSelect value={newProjectAgent} onChange={setNewProjectAgent} inheritLabel="Inherit bucket" /></CatalogField>
              <DrawerActions busy={busy} onCancel={() => setCreating(null)} submit="Create project" />
            </form>
          )}
          {!creating && selectedBucket && !selectedProject && (
            <form onSubmit={saveBucket}>
              <span className="catalogs-overline bucket-accent">Edit bucket</span>
              <h3 ref={detailHeadingRef} tabIndex={-1}>{selectedBucket.name}</h3>
              <p className="catalog-drawer-intro">Controls the defaults available to projects in this bucket.</p>
              <div className="catalog-readonly"><span>Name</span><b>{selectedBucket.name}</b></div>
              <label className="catalog-check catalog-default-check"><input type="radio" name="default-bucket-editor" checked={selectedBucket.isDefault} disabled={selectedBucket.isDefault} onChange={() => void run(() => setDefaultBucket(selectedBucket.id))} /><span>Default bucket</span><small>{selectedBucket.isDefault ? "Current default" : "Make default"}</small></label>
              <fieldset className="catalog-fieldset"><legend>Workers projects may use</legend>{bucketWorkerRows(bucketAllowed, (next) => { if (next.length === 0) { setError("a bucket requires at least one Worker"); return; } setBucketAllowed(next); if (!next.includes(bucketDefaultWorker)) setBucketDefaultWorker(next[0] ?? ""); })}</fieldset>
              <CatalogField label="Default Worker"><select value={bucketDefaultWorker} onChange={(event) => setBucketDefaultWorker(event.target.value)}>{[...new Set([...bucketAllowed, selectedBucket.defaultWorkerId.toString()])].map((id) => <option key={id} value={id} disabled={!state.workers.has(id)}>{workerName(state.workers.get(id), BigInt(id))}</option>)}</select></CatalogField>
              {bucketDroppedProjects.length > 0 && (
                <CatalogField label="Move affected projects to">
                  <select value={bucketReplacement} onChange={(event) => setBucketReplacement(event.target.value)}>
                    <option value="">Default Worker ({workerName(state.workers.get(bucketDefaultWorker), BigInt(bucketDefaultWorker || "0"))})</option>
                    {bucketAllowed.map((id) => <option key={id} value={id} disabled={!state.workers.has(id)}>{workerName(state.workers.get(id), BigInt(id))}</option>)}
                  </select>
                  <small>{bucketDroppedProjects.length} project{bucketDroppedProjects.length === 1 ? "" : "s"} still use{bucketDroppedProjects.length === 1 ? "s" : ""} a Worker you removed: {bucketDroppedProjects.map((project) => project.name).join(", ")}.</small>
                </CatalogField>
              )}
              <CatalogField label="Default Agent"><AgentSelect value={bucketAgent} onChange={setBucketAgent} inheritLabel="System default (Claude)" /></CatalogField>
              <CatalogField label="Permission mode"><select value={bucketPermission} onChange={(event) => setBucketPermission(event.target.value)}>{PERMISSION_CHOICES.map((mode) => <option key={mode} value={String(mode)}>{permissionModeLabel(mode)}</option>)}</select><small>Projects and spawns inherit this unless they set their own.</small></CatalogField>
              <ModelProfileField
                value={bucketProfile}
                onChange={setBucketProfile}
                inheritLabel="Agent account (no profile)"
                profiles={modelProfiles}
                agentDialects={state.agentDialects}
              />
              <DeleteControl label="bucket" confirming={confirmDelete} setConfirming={setConfirmDelete} busy={busy} onDelete={() => void run(() => client.deleteBucket(selectedBucket.id), () => navigate("/settings/projects?catalog=buckets"))} />
              <DrawerActions busy={busy} onCancel={() => navigate("/settings/projects?catalog=buckets")} submit="Save changes" />
            </form>
          )}
          {!creating && selectedProject && selectedProjectBucket && (
            <form onSubmit={saveProject}>
              <span className="catalogs-overline">Edit project</span>
              <h3 ref={detailHeadingRef} tabIndex={-1}>{selectedProject.name}</h3>
              <p className="catalog-drawer-intro">{selectedProjectBucket.name} · Configure where sessions run.</p>
              <div className="catalog-readonly"><span>Name</span><b>{selectedProject.name}</b></div>
              <div className="catalog-readonly"><span>Bucket</span><b>{selectedProjectBucket.name}</b></div>
              <CatalogField label="Absolute path"><DirectoryPicker value={projectPath} onChange={setProjectPath} workerId={projectEditEffectiveWorker} /></CatalogField>
              <fieldset className="catalog-fieldset"><legend>Allowed Workers</legend>{bucketWorkerRows(projectAllowed, (next) => { if (next.length === 0) { setError("a project requires at least one Worker"); return; } setProjectAllowed(next); setProjectWorkerValue(nextProjectDefaultHost(next, projectWorker, selectedProjectBucket.defaultWorkerId.toString())); })}</fieldset>
              <fieldset className="catalog-fieldset"><legend>Worker paths</legend>{projectAllowed.map((id) => {
                const value = projectHostPaths[id] ?? "";
                return (
                  <div className="catalog-host-path" key={id} data-worker-id={id}>
                    <div className="catalog-host-path-head">
                      <span>{workerName(state.workers.get(id), BigInt(id))}</span>
                      <small>{value.trim() ? "Explicit path" : "Inherits project path"}</small>
                    </div>
                    <DirectoryPicker
                      value={value}
                      onChange={(next) => setProjectHostPaths((prev) => ({ ...prev, [id]: next }))}
                      workerId={BigInt(id)}
                      placeholder={projectPath.trim() || selectedProject.path}
                    />
                  </div>
                );
              })}</fieldset>
              <CatalogField label="Default Worker"><select value={projectWorker} onChange={(event) => setProjectWorkerValue(event.target.value)}>{projectAllowed.includes(selectedProjectBucket.defaultWorkerId.toString()) && <option value="">Inherit {selectedProjectBucket.name}</option>}{[...new Set([...projectAllowed, ...(selectedProject.workerId === undefined ? [] : [selectedProject.workerId.toString()])])].map((id) => <option key={id} value={id} disabled={!state.workers.has(id)}>{workerName(state.workers.get(id), BigInt(id))}</option>)}</select></CatalogField>
              <CatalogField label="Default Agent"><AgentSelect value={projectAgent} onChange={setProjectAgent} inheritLabel={`Inherit ${selectedProjectBucket.name} (${agentLabel(resolveAgent(undefined, selectedProjectBucket).agent)})`} /></CatalogField>
              <CatalogField label="Permission mode"><select value={projectPermission} onChange={(event) => setProjectPermission(event.target.value)}><option value="">Inherit {selectedProjectBucket.name} ({permissionModeLabel(resolvePermissionMode(PermissionMode.UNSPECIFIED, PermissionMode.UNSPECIFIED, selectedProjectBucket.permissionMode))})</option>{PERMISSION_CHOICES.map((mode) => <option key={mode} value={String(mode)}>{permissionModeLabel(mode)}</option>)}</select>{permissionFromValue(projectPermission) === PermissionMode.BYPASS && <small className="catalog-warn">Sessions here skip every permission check.</small>}</CatalogField>
              <ModelProfileField
                value={projectProfile}
                onChange={setProjectProfile}
                inheritLabel={`Inherit ${selectedProjectBucket.name} (${profileName(state.modelProfiles, selectedProjectBucket.modelProfileId)})`}
                profiles={modelProfiles}
                agentDialects={state.agentDialects}
              />
              <DeleteControl label="project" confirming={confirmDelete} setConfirming={setConfirmDelete} busy={busy} onDelete={() => void run(() => client.deleteProject(selectedProject.id), () => navigate("/settings/projects?catalog=projects"))} />
              <DrawerActions busy={busy} onCancel={() => navigate("/settings/projects?catalog=projects")} submit="Save changes" />
            </form>
          )}
        </aside>
      )}
    </section>
  );
}

/**
 * Profile picker labelled with the agents each profile covers, so a
 * half-configured profile is visible before a spawn fails.
 */
function ModelProfileField({
  value,
  onChange,
  inheritLabel,
  profiles,
  agentDialects,
}: {
  value: string;
  onChange: (next: string) => void;
  inheritLabel: string;
  profiles: readonly ModelProfile[];
  agentDialects: readonly AgentDialects[];
}) {
  return (
    <CatalogField label="Model Profile">
      <select className="model-profile-select" value={value} onChange={(event) => onChange(event.target.value)}>
        <option value="">{inheritLabel}</option>
        {profiles.map((profile) => {
          const agents = coveredAgents(profile, agentDialects).map(agentLabel);
          return (
            <option key={profile.id.toString()} value={profile.id.toString()}>
              {profile.name} — {agents.length ? agents.join(", ") : "covers no agent"}
            </option>
          );
        })}
      </select>
    </CatalogField>
  );
}

function CatalogField({ label, children }: { label: string; children: ReactNode }) {
  return <label className="catalog-field"><span>{label}</span>{children}</label>;
}

function CatalogState({ kind, title, body, action }: { kind: "loading" | "empty"; title: string; body: string; action?: ReactNode }) {
  return <div className={`catalog-state is-${kind}`} role={kind === "loading" ? "status" : undefined}><span aria-hidden="true">{kind === "loading" ? "···" : "◇"}</span><h3>{title}</h3><p>{body}</p>{action}</div>;
}

function CatalogError({ message, onDismiss }: { message: string; onDismiss: () => void }) {
  return <div className="catalog-error" role="alert"><span>{message}</span><button type="button" className="btn" onClick={onDismiss}>Dismiss</button></div>;
}

function DrawerActions({ busy, onCancel, submit }: { busy: boolean; onCancel: () => void; submit: string }) {
  return <div className="catalog-drawer-actions"><button type="button" className="btn" onClick={onCancel}>Cancel</button><button type="submit" className="btn btn-primary" disabled={busy}>{busy ? "Saving…" : submit}</button></div>;
}

function DeleteControl({ label, confirming, setConfirming, busy, onDelete }: { label: "bucket" | "project"; confirming: boolean; setConfirming: (value: boolean) => void; busy: boolean; onDelete: () => void }) {
  return <div className="catalog-danger"><div><b>Delete {label}</b><small>{label === "bucket" ? "Projects must be removed first." : "Running sessions prevent deletion."}</small></div>{confirming ? <div className="catalog-danger-confirm"><span>Delete permanently?</span><button type="button" className="btn" onClick={() => setConfirming(false)}>Keep</button><button type="button" className="btn btn-danger" disabled={busy} onClick={onDelete}>Confirm delete</button></div> : <button type="button" className="btn btn-danger" onClick={() => setConfirming(true)}>Delete…</button>}</div>;
}
