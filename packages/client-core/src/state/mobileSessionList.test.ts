import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import {
  SessionRole,
  SessionSchema,
  SessionState,
  AgentKind,
  ProjectSchema,
} from "../gen/pm/v1/pm_pb";
import {
  buildMobileSessionList,
  bySalience,
  filterSessions,
  needsAttention,
  groupNeedsAttention,
  groupLastActiveAt,
  projectIdsForBucket,
} from "./mobileSessionList";

const MINUTE = 60_000;

function makeSession(
  id: bigint,
  state: SessionState,
  overrides: {
    role?: number;
    spawnedBy?: bigint;
    lastActivity?: bigint;
    created?: bigint;
  } = {},
) {
  return create(SessionSchema, {
    id,
    projectId: 1n,
    agent: AgentKind.CLAUDE_CODE,
    state,
    taskTitle: `task ${id}`,
    createdAtUnixMs: overrides.created ?? 1_000n * id,
    role: overrides.role ?? SessionRole.UNSPECIFIED,
    spawnedBySessionId: overrides.spawnedBy,
    lastActivityAtUnixMs: overrides.lastActivity ?? 0n,
  });
}

// ---------------------------------------------------------------------------
// needsAttention
// ---------------------------------------------------------------------------

describe("needsAttention", () => {
  it("returns true for NEEDS_INPUT", () => {
    expect(needsAttention(makeSession(1n, SessionState.NEEDS_INPUT))).toBe(true);
  });

  it("returns true for FAILED", () => {
    expect(needsAttention(makeSession(1n, SessionState.FAILED))).toBe(true);
  });

  it("returns false for WORKING", () => {
    expect(needsAttention(makeSession(1n, SessionState.WORKING))).toBe(false);
  });

  it("returns false for IDLE", () => {
    expect(needsAttention(makeSession(1n, SessionState.IDLE))).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// groupNeedsAttention
// ---------------------------------------------------------------------------

describe("groupNeedsAttention", () => {
  it("returns true when any worker needs input", () => {
    const workers = [
      makeSession(1n, SessionState.WORKING),
      makeSession(2n, SessionState.NEEDS_INPUT),
    ];
    expect(groupNeedsAttention(workers)).toBe(true);
  });

  it("returns true when any worker has failed", () => {
    const workers = [
      makeSession(1n, SessionState.WORKING),
      makeSession(2n, SessionState.FAILED),
    ];
    expect(groupNeedsAttention(workers)).toBe(true);
  });

  it("returns false when no worker needs attention", () => {
    const workers = [
      makeSession(1n, SessionState.WORKING),
      makeSession(2n, SessionState.IDLE),
    ];
    expect(groupNeedsAttention(workers)).toBe(false);
  });

  it("returns false for empty workers array", () => {
    expect(groupNeedsAttention([])).toBe(false);
  });
});

// ---------------------------------------------------------------------------
// groupLastActiveAt
// ---------------------------------------------------------------------------

describe("groupLastActiveAt", () => {
  it("takes the maximum activity across supervisor and workers", () => {
    const sup = makeSession(1n, SessionState.IDLE, { lastActivity: BigInt(5 * MINUTE) });
    const w1 = makeSession(2n, SessionState.WORKING, { lastActivity: BigInt(9 * MINUTE) });
    const w2 = makeSession(3n, SessionState.WORKING, { lastActivity: BigInt(3 * MINUTE) });
    expect(groupLastActiveAt(sup, [w1, w2])).toBe(9 * MINUTE);
  });

  it("falls back to creation time when no activity timestamp", () => {
    const sup = makeSession(1n, SessionState.IDLE, { created: BigInt(2 * MINUTE) });
    expect(groupLastActiveAt(sup, [])).toBe(2 * MINUTE);
  });

  it("considers only the supervisor when there are no workers", () => {
    const sup = makeSession(1n, SessionState.IDLE, { lastActivity: BigInt(7 * MINUTE) });
    expect(groupLastActiveAt(sup, [])).toBe(7 * MINUTE);
  });
});

// ---------------------------------------------------------------------------
// bySalience (minute-bucketed ordering)
// ---------------------------------------------------------------------------

describe("bySalience", () => {
  it("ranks needs-input above working", () => {
    const a = makeSession(1n, SessionState.NEEDS_INPUT, { lastActivity: BigInt(1 * MINUTE) });
    const b = makeSession(2n, SessionState.WORKING, { lastActivity: BigInt(9 * MINUTE) });
    expect(bySalience(a, b)).toBeLessThan(0);
  });

  it("ranks more recently active sessions first within the same state", () => {
    const a = makeSession(1n, SessionState.WORKING, { lastActivity: BigInt(9 * MINUTE) });
    const b = makeSession(2n, SessionState.WORKING, { lastActivity: BigInt(3 * MINUTE) });
    expect(bySalience(a, b)).toBeLessThan(0);
  });

  it("does not reorder sessions within the same minute bucket", () => {
    const a = makeSession(1n, SessionState.WORKING, {
      lastActivity: BigInt(3 * MINUTE + 59_000),
    });
    const b = makeSession(2n, SessionState.WORKING, {
      lastActivity: BigInt(3 * MINUTE),
      created: BigInt(3 * MINUTE), // newer creation
    });
    // Both in same minute bucket; b has a higher createdAtUnixMs so sorts first
    expect(bySalience(a, b)).toBeGreaterThan(0);
  });

  it("ranks exited below working", () => {
    const a = makeSession(1n, SessionState.EXITED);
    const b = makeSession(2n, SessionState.WORKING);
    expect(bySalience(a, b)).toBeGreaterThan(0);
  });
});

// ---------------------------------------------------------------------------
// buildMobileSessionList — grouping
// ---------------------------------------------------------------------------

describe("buildMobileSessionList — grouping", () => {
  it("groups workers under their supervisor", () => {
    const sup = makeSession(1n, SessionState.IDLE, { role: SessionRole.SUPERVISOR });
    const w1 = makeSession(2n, SessionState.WORKING, {
      role: SessionRole.WORKER,
      spawnedBy: 1n,
    });
    const w2 = makeSession(3n, SessionState.WORKING, {
      role: SessionRole.WORKER,
      spawnedBy: 1n,
    });

    const entries = buildMobileSessionList([sup, w1, w2]);
    expect(entries).toHaveLength(1);
    expect(entries[0].kind).toBe("group");
    if (entries[0].kind === "group") {
      expect(entries[0].supervisor.id).toBe(1n);
      expect(entries[0].workers).toHaveLength(2);
    }
  });

  it("shows unsupervised sessions as standalone", () => {
    const standalone = makeSession(1n, SessionState.WORKING);
    const entries = buildMobileSessionList([standalone]);
    expect(entries).toHaveLength(1);
    expect(entries[0].kind).toBe("standalone");
  });

  it("treats workers whose supervisor is absent as standalone", () => {
    const orphanWorker = makeSession(2n, SessionState.WORKING, {
      role: SessionRole.WORKER,
      spawnedBy: 99n, // supervisor not in list
    });
    const entries = buildMobileSessionList([orphanWorker]);
    expect(entries).toHaveLength(1);
    expect(entries[0].kind).toBe("standalone");
  });

  it("intermixes groups and standalone sessions by activity", () => {
    const sup = makeSession(1n, SessionState.IDLE, {
      role: SessionRole.SUPERVISOR,
      lastActivity: BigInt(2 * MINUTE),
    });
    const worker = makeSession(2n, SessionState.WORKING, {
      role: SessionRole.WORKER,
      spawnedBy: 1n,
      lastActivity: BigInt(2 * MINUTE),
    });
    const standalone = makeSession(3n, SessionState.WORKING, {
      lastActivity: BigInt(9 * MINUTE),
    });

    const entries = buildMobileSessionList([sup, worker, standalone]);
    expect(entries).toHaveLength(2);
    // Standalone is more recently active, should come first
    expect(entries[0].kind).toBe("standalone");
    expect(entries[1].kind).toBe("group");
  });
});

// ---------------------------------------------------------------------------
// buildMobileSessionList — attention propagation
// ---------------------------------------------------------------------------

describe("buildMobileSessionList — attention propagation", () => {
  it("propagates needs-input from a worker to the group", () => {
    const sup = makeSession(1n, SessionState.IDLE, { role: SessionRole.SUPERVISOR });
    const w = makeSession(2n, SessionState.NEEDS_INPUT, {
      role: SessionRole.WORKER,
      spawnedBy: 1n,
    });

    const entries = buildMobileSessionList([sup, w]);
    expect(entries).toHaveLength(1);
    expect(entries[0].kind).toBe("group");
    if (entries[0].kind === "group") {
      expect(entries[0].needsAttention).toBe(true);
    }
  });

  it("propagates failed from a worker to the group", () => {
    const sup = makeSession(1n, SessionState.IDLE, { role: SessionRole.SUPERVISOR });
    const w = makeSession(2n, SessionState.FAILED, {
      role: SessionRole.WORKER,
      spawnedBy: 1n,
    });

    const entries = buildMobileSessionList([sup, w]);
    if (entries[0].kind === "group") {
      expect(entries[0].needsAttention).toBe(true);
    }
  });

  it("propagates needs-input from the supervisor itself to the group", () => {
    const sup = makeSession(1n, SessionState.NEEDS_INPUT, { role: SessionRole.SUPERVISOR });
    const w = makeSession(2n, SessionState.IDLE, {
      role: SessionRole.WORKER,
      spawnedBy: 1n,
    });

    const entries = buildMobileSessionList([sup, w]);
    expect(entries).toHaveLength(1);
    expect(entries[0].kind).toBe("group");
    if (entries[0].kind === "group") {
      expect(entries[0].needsAttention).toBe(true);
    }
  });

  it("a supervisor in NEEDS_INPUT with idle workers sorts ahead of an idle standalone", () => {
    const sup = makeSession(1n, SessionState.NEEDS_INPUT, {
      role: SessionRole.SUPERVISOR,
      lastActivity: BigInt(1 * MINUTE),
    });
    const w = makeSession(2n, SessionState.IDLE, {
      role: SessionRole.WORKER,
      spawnedBy: 1n,
      lastActivity: BigInt(1 * MINUTE),
    });
    const standalone = makeSession(3n, SessionState.IDLE, {
      lastActivity: BigInt(9 * MINUTE),
    });

    const entries = buildMobileSessionList([sup, w, standalone]);
    expect(entries).toHaveLength(2);
    // The group's state rank is 0 (supervisor NEEDS_INPUT), standalone is 1 (IDLE)
    expect(entries[0].kind).toBe("group");
    if (entries[0].kind === "group") {
      expect(entries[0].needsAttention).toBe(true);
    }
  });

  it("does not propagate attention when all workers are healthy", () => {
    const sup = makeSession(1n, SessionState.IDLE, { role: SessionRole.SUPERVISOR });
    const w = makeSession(2n, SessionState.WORKING, {
      role: SessionRole.WORKER,
      spawnedBy: 1n,
    });

    const entries = buildMobileSessionList([sup, w]);
    if (entries[0].kind === "group") {
      expect(entries[0].needsAttention).toBe(false);
    }
  });
});

// ---------------------------------------------------------------------------
// buildMobileSessionList — ordering
// ---------------------------------------------------------------------------

describe("buildMobileSessionList — ordering", () => {
  it("sorts entries by most recent activity descending", () => {
    const s1 = makeSession(1n, SessionState.WORKING, { lastActivity: BigInt(2 * MINUTE) });
    const s2 = makeSession(2n, SessionState.WORKING, { lastActivity: BigInt(8 * MINUTE) });
    const s3 = makeSession(3n, SessionState.WORKING, { lastActivity: BigInt(5 * MINUTE) });

    const entries = buildMobileSessionList([s1, s2, s3]);
    const ids = entries.map((e) => (e.kind === "standalone" ? e.session.id : e.supervisor.id));
    expect(ids).toEqual([2n, 3n, 1n]);
  });

  it("uses group-level activity for ordering groups", () => {
    // Group with a very recently active worker should sort above a standalone
    const sup = makeSession(1n, SessionState.IDLE, {
      role: SessionRole.SUPERVISOR,
      lastActivity: BigInt(1 * MINUTE),
    });
    const w = makeSession(2n, SessionState.WORKING, {
      role: SessionRole.WORKER,
      spawnedBy: 1n,
      lastActivity: BigInt(10 * MINUTE),
    });
    const standalone = makeSession(3n, SessionState.WORKING, {
      lastActivity: BigInt(5 * MINUTE),
    });

    const entries = buildMobileSessionList([sup, w, standalone]);
    expect(entries[0].kind).toBe("group"); // group has activity at 10m
    expect(entries[1].kind).toBe("standalone"); // standalone at 5m
  });

  it("sorts workers within a group by salience", () => {
    const sup = makeSession(1n, SessionState.IDLE, { role: SessionRole.SUPERVISOR });
    const w1 = makeSession(2n, SessionState.WORKING, {
      role: SessionRole.WORKER,
      spawnedBy: 1n,
      lastActivity: BigInt(3 * MINUTE),
    });
    const w2 = makeSession(3n, SessionState.NEEDS_INPUT, {
      role: SessionRole.WORKER,
      spawnedBy: 1n,
      lastActivity: BigInt(1 * MINUTE),
    });

    const entries = buildMobileSessionList([sup, w1, w2]);
    if (entries[0].kind === "group") {
      // needs-input ranks above working
      expect(entries[0].workers[0].id).toBe(3n);
      expect(entries[0].workers[1].id).toBe(2n);
    }
  });

  it("a group with a needs-input worker ranks above a working standalone", () => {
    const sup = makeSession(1n, SessionState.IDLE, {
      role: SessionRole.SUPERVISOR,
      lastActivity: BigInt(1 * MINUTE),
    });
    const w = makeSession(2n, SessionState.NEEDS_INPUT, {
      role: SessionRole.WORKER,
      spawnedBy: 1n,
      lastActivity: BigInt(1 * MINUTE),
    });
    const standalone = makeSession(3n, SessionState.WORKING, {
      lastActivity: BigInt(9 * MINUTE),
    });

    const entries = buildMobileSessionList([sup, w, standalone]);
    // The group's best state rank is 0 (needs-input), standalone is 1 (working)
    expect(entries[0].kind).toBe("group");
  });
});

// ---------------------------------------------------------------------------
// filterSessions & projectIdsForBucket
// ---------------------------------------------------------------------------

function makeSessionWithProject(id: bigint, projectId: bigint, state = SessionState.WORKING) {
  return create(SessionSchema, {
    id,
    projectId,
    agent: AgentKind.CLAUDE_CODE,
    state,
    taskTitle: `task ${id}`,
    createdAtUnixMs: 1_000n * id,
  });
}

describe("projectIdsForBucket", () => {
  it("collects project ids belonging to the given bucket", () => {
    const projects = [
      create(ProjectSchema, { id: 10n, bucketId: 1n, name: "a", path: "/a" }),
      create(ProjectSchema, { id: 20n, bucketId: 2n, name: "b", path: "/b" }),
      create(ProjectSchema, { id: 30n, bucketId: 1n, name: "c", path: "/c" }),
    ];
    const ids = projectIdsForBucket(projects, "1");
    expect(ids).toEqual(new Set(["10", "30"]));
  });

  it("returns an empty set when no projects match", () => {
    const projects = [
      create(ProjectSchema, { id: 10n, bucketId: 1n, name: "a", path: "/a" }),
    ];
    expect(projectIdsForBucket(projects, "99")).toEqual(new Set());
  });
});

describe("filterSessions", () => {
  const sessions = [
    makeSessionWithProject(1n, 10n),
    makeSessionWithProject(2n, 20n),
    makeSessionWithProject(3n, 10n),
    makeSessionWithProject(4n, 30n),
  ];

  it("returns all sessions when no filter is active", () => {
    const result = filterSessions(sessions, {}, null);
    expect(result).toHaveLength(4);
  });

  it("filters by project id", () => {
    const result = filterSessions(sessions, { projectId: "10" }, null);
    expect(result.map((s) => s.id)).toEqual([1n, 3n]);
  });

  it("filters by bucket using project set", () => {
    const projectsInBucket = new Set(["10", "30"]);
    const result = filterSessions(sessions, { bucketId: "1" }, projectsInBucket);
    expect(result.map((s) => s.id)).toEqual([1n, 3n, 4n]);
  });

  it("project filter takes precedence over bucket filter", () => {
    const projectsInBucket = new Set(["10", "30"]);
    const result = filterSessions(
      sessions,
      { bucketId: "1", projectId: "20" },
      projectsInBucket,
    );
    expect(result.map((s) => s.id)).toEqual([2n]);
  });
});
