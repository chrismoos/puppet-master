import type { Project, Session } from "../gen/pm/v1/pm_pb";
import { LOCAL_WORKER_ID } from "../format";

export interface SessionLaunchFolder {
  label: string;
  description: string;
}

export type SessionLaunchPathKind =
  | "missing"
  | "opaque"
  | "project-root"
  | "project-nested"
  | "recorded";

export interface SessionLaunchMetadata {
  origin: "local" | "remote";
  path: string | null;
  pathKind: SessionLaunchPathKind;
  pathContext: string;
}

function normalizedPath(path: string): string {
  const normalized = path.trim().replaceAll("\\", "/");
  if (/^[A-Za-z]:\/$/.test(normalized) || normalized === "/") return normalized;
  return normalized.replace(/\/+$/, "");
}

function comparablePath(path: string): string {
  const normalized = normalizedPath(path);
  return /^[A-Za-z]:\//.test(normalized) ? normalized.toLowerCase() : normalized;
}

function basename(path: string): string {
  const normalized = normalizedPath(path);
  if (normalized === "/" || /^[A-Za-z]:\/$/.test(normalized)) return normalized;
  return normalized.slice(normalized.lastIndexOf("/") + 1);
}

function relativeFolder(cwd: string, root: string): string | null {
  const normalizedCwd = normalizedPath(cwd);
  const normalizedRoot = normalizedPath(root);
  const comparableCwd = comparablePath(cwd);
  const comparableRoot = comparablePath(root);
  if (!normalizedRoot || comparableCwd === comparableRoot) return null;
  const prefix = comparableRoot === "/" ? "/" : `${comparableRoot}/`;
  if (!comparableCwd.startsWith(prefix)) return null;
  return normalizedCwd.slice(prefix.length);
}

/**
 * Classifies the persisted spawn-time path for detailed presentation.
 * This deliberately does not touch the filesystem: a recorded path can be
 * inspected even when its worker is offline or the directory was deleted.
 */
export function sessionLaunchMetadata(
  session: Pick<Session, "cwd" | "workerId">,
  project: Pick<Project, "name" | "path"> | undefined,
): SessionLaunchMetadata {
  const cwd = normalizedPath(session.cwd);
  const projectPath = normalizedPath(project?.path ?? "");
  const origin = session.workerId === LOCAL_WORKER_ID ? "local" : "remote";

  if (!cwd) {
    return {
      origin,
      path: null,
      pathKind: "missing",
      pathContext: "Not recorded for this session (common for older sessions).",
    };
  }

  if (basename(cwd) === "(*)") {
    return {
      origin,
      path: session.cwd,
      pathKind: "opaque",
      pathContext: "Opaque legacy value. The “(*)” suffix has no repository-status meaning.",
    };
  }

  if (projectPath && comparablePath(cwd) === comparablePath(projectPath)) {
    return {
      origin,
      path: session.cwd,
      pathKind: "project-root",
      pathContext: origin === "remote"
        ? "Matches the configured controller-side project path; remote mappings may use another root."
        : "Configured project root.",
    };
  }

  const nested = projectPath ? relativeFolder(cwd, projectPath) : null;
  if (nested) {
    return {
      origin,
      path: session.cwd,
      pathKind: "project-nested",
      pathContext: `Nested folder within the configured project: ${nested}`,
    };
  }

  return {
    origin,
    path: session.cwd,
    pathKind: "recorded",
    pathContext: project
      ? "Recorded launch path outside the configured project path."
      : "Recorded launch path; the configured project is unavailable.",
  };
}

/**
 * Returns sidebar launch-folder metadata only when it adds context.
 *
 * Session.cwd is the effective worker-local directory fixed at spawn time. It
 * is not live filesystem state and carries no git, worktree, or dirty marker;
 * a literal "(*)" is therefore opaque stored path data, not repository status.
 * Local project-root launches are omitted. Remote paths are shown when they
 * differ from the controller's project path because the web snapshot does not
 * expose the worker's project-root mapping.
 */
export function sessionLaunchFolder(
  session: Pick<Session, "cwd" | "workerId">,
  project: Pick<Project, "name" | "path"> | undefined,
): SessionLaunchFolder | null {
  const cwd = normalizedPath(session.cwd);
  const projectPath = normalizedPath(project?.path ?? "");

  if (!cwd || basename(cwd) === "(*)") {
    return {
      label: "folder unavailable",
      description: cwd
        ? `The recorded launch folder, ${session.cwd}, ends in the opaque marker “(*)”. Puppet Master has no repository-status meaning for this value.`
        : "The launch folder was not recorded for this session.",
    };
  }

  if (projectPath && comparablePath(cwd) === comparablePath(projectPath)) return null;

  const remote = session.workerId !== LOCAL_WORKER_ID;
  const relative = !remote && projectPath ? relativeFolder(cwd, projectPath) : null;
  const shortPath = relative || basename(cwd) || cwd;
  const projectContext = project
    ? ` The configured project is ${project.name} at ${project.path || "an unrecorded path"}.`
    : "";

  if (remote) {
    return {
      label: `remote folder · ${shortPath}`,
      description: `Worker-local launch folder: ${session.cwd}.${projectContext} Remote project mappings may use a different root. Puppet Master does not infer git status from this path.`,
    };
  }

  return {
    label: `folder · ${shortPath}`,
    description: `Launch folder: ${session.cwd}.${projectContext} This recorded spawn-time path may no longer exist; Puppet Master does not infer git status from it.`,
  };
}
