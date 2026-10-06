import { sessionDisplayName, sessionEnded, sessionLastActive } from "../format";
import type { Item, Project, Session } from "../gen/pm/v1/pm_pb";

export type ItemResponseTarget =
  | { kind: "session"; sessionId: bigint }
  | { kind: "newSupervisor"; projectId: bigint }
  | { kind: "replyOnly" };

export interface ItemResponseOption {
  target: ItemResponseTarget;
  label: string;
}

export interface ItemResponseOptions {
  existingSupervisors: ItemResponseOption[];
  newSupervisors: ItemResponseOption[];
  replyOnly: ItemResponseOption;
}

function lastActivity(session: Session): bigint {
  return session.lastActivityAtUnixMs;
}

function byLastActivity(a: Session, b: Session): number {
  const activityA = lastActivity(a);
  const activityB = lastActivity(b);
  if (activityA !== activityB) return activityA > activityB ? -1 : 1;
  if (a.id === b.id) return 0;
  return a.id > b.id ? -1 : 1;
}

function isLiveSupervisor(session: Session): boolean {
  return session.supervisorApi && !sessionEnded(session);
}

function bucketProjects(item: Item, projects: Iterable<Project>): Project[] {
  return [...projects]
    .filter((project) => project.bucketId === item.bucketId)
    .sort((a, b) => a.name.localeCompare(b.name) || Number(a.id - b.id));
}

function supervisorsInBucket(
  item: Item,
  sessions: Iterable<Session>,
  projects: Iterable<Project>,
): Array<{ session: Session; project: Project }> {
  const projectById = new Map(
    bucketProjects(item, projects).map((project) => [project.id.toString(), project]),
  );
  return [...sessions]
    .filter(isLiveSupervisor)
    .map((session) => ({ session, project: projectById.get(session.projectId.toString()) }))
    .filter((entry): entry is { session: Session; project: Project } => entry.project !== undefined)
    .sort((a, b) => byLastActivity(a.session, b.session));
}

export function resolvePrimaryResponseTarget(
  item: Item,
  sessions: Iterable<Session>,
  projects: Iterable<Project>,
): ItemResponseTarget | null {
  const sessionList = [...sessions];
  const linkedIds = new Set(item.sessionIds.map((id) => id.toString()));
  const linked = sessionList
    .filter((session) => linkedIds.has(session.id.toString()) && isLiveSupervisor(session))
    .sort(byLastActivity);
  if (linked[0]) return { kind: "session", sessionId: linked[0].id };

  const bucketSupervisor = supervisorsInBucket(item, sessionList, projects)[0];
  return bucketSupervisor
    ? { kind: "session", sessionId: bucketSupervisor.session.id }
    : null;
}

export function buildItemResponseOptions(
  item: Item,
  sessions: Iterable<Session>,
  projects: Iterable<Project>,
  nowMs: number,
): ItemResponseOptions {
  const projectList = bucketProjects(item, projects);
  const existingSupervisors = supervisorsInBucket(item, sessions, projectList).map(
    ({ session, project }) => ({
      target: { kind: "session", sessionId: session.id } as const,
      label: `${sessionDisplayName(session)} · ${project.name} · ${sessionLastActive(session, nowMs) || "no activity yet"}`,
    }),
  );
  const newSupervisors = projectList.map((project) => ({
    target: { kind: "newSupervisor", projectId: project.id } as const,
    label: `New supervisor in ${project.name}`,
  }));

  return {
    existingSupervisors,
    newSupervisors,
    replyOnly: { target: { kind: "replyOnly" }, label: "Reply only (no session)" },
  };
}
