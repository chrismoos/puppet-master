import { create } from "@bufbuild/protobuf";
import { createElement, type ReactElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { BucketSchema, ContextFieldSchema, ContextKind, SessionGitSchema, SessionRole, SessionSchema, SessionState, type Session } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import type { SidebarBucket, SidebarModel } from "@puppet-master/client-core/state/sidebar";
import {
  BoardLink,
  DescendantAttention,
  filterSidebarByState,
  needsInputAttention,
  nextSupervisorFilter,
  rollupCounts,
  rollupState,
  StatusTallies,
  sessionAccessibleLabel,
  SessionBranch,
  SessionGlance,
  sessionRowClass,
  SessionRoleIcon,
  SessionRowText,
  supervisorTree,
  SupervisorNode,
  visibleGlanceFields,
  type RollupState,
} from "./Sidebar";

describe("SessionRowText", () => {
  it("names the row by goal with the headline as a second line and keeps the state detail", () => {
    const markup = renderToStaticMarkup(createElement(SessionRowText, {
      session: create(SessionSchema, {
        id: 5n,
        goal: "Optimizing femtocell software",
        headline: "Editing femtocell.rs 2/3",
        stateDetail: "worker offline",
      }),
    }));

    expect(markup).toBe(
      '<span class="sb-session-title">Optimizing femtocell software</span>'
        + '<span class="sb-session-status">Editing femtocell.rs 2/3</span>'
        + '<span class="sb-session-detail">worker offline</span>',
    );
  });

  it("omits the second line when the name already is the headline", () => {
    const markup = renderToStaticMarkup(createElement(SessionRowText, {
      session: create(SessionSchema, { id: 5n, headline: "Editing femtocell.rs" }),
    }));

    expect(markup).toBe('<span class="sb-session-title">Editing femtocell.rs</span>');
  });
});

describe("sessionAccessibleLabel", () => {
  it("combines the current display name and NeedsInput status", () => {
    const session = create(SessionSchema, {
      id: 120n,
      headline: "Choose the rollout window",
      state: SessionState.NEEDS_INPUT,
    });

    expect(sessionAccessibleLabel(session)).toBe("Choose the rollout window, needs input, viewed but unanswered");
    session.needsInputUnseen = true;
    expect(sessionAccessibleLabel(session)).toBe("Choose the rollout window, needs input, unseen attention");
  });

  it("distinguishes a freshly finished idle session from a viewed one", () => {
    const session = create(SessionSchema, {
      id: 120n,
      headline: "Migrate the auth tables",
      state: SessionState.IDLE,
      idleUnseen: true,
    });

    expect(sessionAccessibleLabel(session)).toBe("Migrate the auth tables, idle, freshly finished");
    session.idleUnseen = false;
    expect(sessionAccessibleLabel(session)).toBe("Migrate the auth tables, idle");
  });

  it("speaks the derived state so the label never disagrees with the dot", () => {
    const session = create(SessionSchema, {
      id: 120n,
      headline: "Supervising the rollout",
      role: SessionRole.SUPERVISOR,
      state: SessionState.IDLE,
      idleUnseen: true,
    });

    expect(sessionAccessibleLabel(session, SessionState.WORKING))
      .toBe("Supervising the rollout, working");
  });
});

describe("sessionRowClass", () => {
  it("marks a freshly finished idle session on child and top-level rows", () => {
    const fresh = create(SessionSchema, { state: SessionState.IDLE, idleUnseen: true });
    expect(sessionRowClass(fresh, { child: true })).toBe("sb-session-row sb-child is-idle-fresh");
    expect(sessionRowClass(fresh)).toBe("sb-session-row is-idle-fresh");
  });

  it("leaves a viewed idle session with the plain idle look", () => {
    const seen = create(SessionSchema, { state: SessionState.IDLE, idleUnseen: false });
    expect(sessionRowClass(seen, { child: true })).toBe("sb-session-row sb-child");
    expect(sessionRowClass(seen, { selected: true })).toBe("sb-session-row is-selected");
  });

  it("keeps the amber attention pair for needs-input rows", () => {
    const unseen = create(SessionSchema, {
      state: SessionState.NEEDS_INPUT,
      needsInputUnseen: true,
    });
    expect(sessionRowClass(unseen)).toBe("sb-session-row is-needs-input is-attention-unseen");
    unseen.needsInputUnseen = false;
    expect(sessionRowClass(unseen, { child: true })).toBe(
      "sb-session-row sb-child is-needs-input is-attention-seen",
    );
  });

  it("never marks a non-idle session fresh even with a stale bit", () => {
    const working = create(SessionSchema, { state: SessionState.WORKING, idleUnseen: true });
    expect(sessionRowClass(working)).toBe("sb-session-row");
  });

  it("drops the fresh-idle mark once the row renders as running", () => {
    const fresh = create(SessionSchema, { state: SessionState.IDLE, idleUnseen: true });
    expect(sessionRowClass(fresh, { display: SessionState.WORKING })).toBe("sb-session-row");
  });
});

describe("NeedsInput attention", () => {
  it("aggregates unseen and seen descendants separately", () => {
    const sessions = [
      create(SessionSchema, { state: SessionState.NEEDS_INPUT, needsInputUnseen: true }),
      create(SessionSchema, { state: SessionState.NEEDS_INPUT, needsInputUnseen: false }),
      create(SessionSchema, { state: SessionState.WORKING, needsInputUnseen: true }),
    ];
    expect(needsInputAttention(sessions)).toEqual({ unseen: 1, seen: 1 });

    const unseen = renderToStaticMarkup(createElement(DescendantAttention, {
      attention: { unseen: 1, seen: 1 },
    }));
    expect(unseen).toContain("is-unseen");
    expect(unseen).toContain("2 descendant sessions need input, 1 unseen");

    const seen = renderToStaticMarkup(createElement(DescendantAttention, {
      attention: { unseen: 0, seen: 2 },
    }));
    expect(seen).toContain("is-seen");
    expect(seen).toContain("2 viewed descendant sessions still need input");
  });
});

function worker(
  id: bigint,
  state: SessionState,
  options: { spawnedBy?: bigint; projectId?: bigint } = {},
) {
  return create(SessionSchema, {
    id,
    projectId: options.projectId ?? 10n,
    role: SessionRole.WORKER,
    state,
    taskTitle: `task ${id}`,
    spawnedBySessionId: options.spawnedBy,
  });
}

function supervisor(id: bigint, projectId = 10n) {
  return create(SessionSchema, {
    id,
    projectId,
    role: SessionRole.SUPERVISOR,
    state: SessionState.WORKING,
    taskTitle: `supervisor ${id}`,
  });
}

const byId = (a: { id: bigint }, b: { id: bigint }) => Number(a.id - b.id);

describe("supervisorTree", () => {
  const bucket = create(BucketSchema, { id: 1n, name: "work" });

  it("hangs spawned sessions off their supervisor and leaves the rest at bucket level", () => {
    const boss = supervisor(2n);
    const spawned = worker(5n, SessionState.NEEDS_INPUT, { spawnedBy: 2n });
    const loose = worker(6n, SessionState.WORKING);
    const model: SidebarBucket = {
      bucket,
      supervisors: [boss],
      sessions: [spawned, loose],
    };

    const tree = supervisorTree(model, byId);

    expect(tree.supervisors.map(({ supervisor: s }) => s.id)).toEqual([2n]);
    expect(tree.supervisors[0].workers.map((s) => s.id)).toEqual([5n]);
    expect(tree.loose.map((s) => s.id)).toEqual([6n]);
  });

  it("claims a worker whose project differs from its supervisor's", () => {
    const boss = supervisor(2n, 10n);
    const elsewhere = worker(7n, SessionState.WORKING, { spawnedBy: 2n, projectId: 11n });
    const model: SidebarBucket = {
      bucket,
      supervisors: [boss],
      sessions: [elsewhere],
    };

    const tree = supervisorTree(model, byId);

    expect(tree.supervisors[0].workers.map((s) => s.id)).toEqual([7n]);
    expect(tree.loose).toEqual([]);
  });

  it("leaves a session at bucket level when no rendered supervisor spawned it", () => {
    const orphaned = worker(8n, SessionState.WORKING, { spawnedBy: 99n });
    const model: SidebarBucket = {
      bucket,
      supervisors: [],
      sessions: [orphaned],
    };

    expect(supervisorTree(model, byId).loose.map((s) => s.id)).toEqual([8n]);
  });

  it("orders a supervisor's workers with the model's own comparator", () => {
    const boss = supervisor(2n);
    const model: SidebarBucket = {
      bucket,
      supervisors: [boss],
      sessions: [
        worker(9n, SessionState.WORKING, { spawnedBy: 2n }),
        worker(4n, SessionState.WORKING, { spawnedBy: 2n }),
        worker(6n, SessionState.WORKING, { spawnedBy: 2n }),
      ],
    };

    expect(supervisorTree(model, byId).supervisors[0].workers.map((s) => s.id)).toEqual([4n, 6n, 9n]);
  });
});

describe("rollup states", () => {
  it.each([
    [SessionState.NEEDS_INPUT, "needs-input"],
    [SessionState.IDLE, "idle"],
    [SessionState.FAILED, "failed"],
    [SessionState.EXITED, "ended"],
    [SessionState.WORKING, "working"],
    [SessionState.STARTING, "working"],
    [SessionState.AWAITING_WORKER, "working"],
  ])("maps %s to its rollup state", (state, expected) => {
    expect(rollupState(state)).toBe(expected);
  });

  it("counts a supervisor's workers by rollup state", () => {
    expect(rollupCounts([
      worker(1n, SessionState.WORKING),
      worker(2n, SessionState.NEEDS_INPUT),
      worker(3n, SessionState.NEEDS_INPUT),
      worker(4n, SessionState.IDLE),
      worker(5n, SessionState.EXITED),
    ])).toEqual({ working: 1, "needs-input": 2, idle: 1, failed: 0, ended: 1 });
  });
});

describe("nextSupervisorFilter", () => {
  it("opens a supervisor filtered to the picked state", () => {
    expect([...nextSupervisorFilter(new Map(), "2", "needs-input")]).toEqual([["2", "needs-input"]]);
  });

  it("returns to the full list when the active state is picked again", () => {
    const filters = new Map<string, RollupState>([["2", "needs-input"]]);
    expect([...nextSupervisorFilter(filters, "2", "needs-input")]).toEqual([]);
  });

  it("switches straight to another state and leaves other supervisors alone", () => {
    const filters = new Map<string, RollupState>([["2", "needs-input"], ["3", "idle"]]);
    expect([...nextSupervisorFilter(filters, "2", "failed")]).toEqual([["2", "failed"], ["3", "idle"]]);
  });
});

describe("SupervisorNode", () => {
  const workers = [
    worker(1n, SessionState.WORKING),
    worker(2n, SessionState.WORKING),
    worker(3n, SessionState.NEEDS_INPUT),
    worker(4n, SessionState.NEEDS_INPUT),
    worker(5n, SessionState.IDLE),
    worker(6n, SessionState.FAILED),
  ];

  const render = (overrides: Partial<Parameters<typeof SupervisorNode>[0]> = {}) =>
    renderToStaticMarkup(createElement(SupervisorNode, {
      row: createElement("div", { className: "sb-session-row" }),
      workers,
      label: "supervisor 2",
      collapsed: false,
      filter: null,
      onToggleCollapse: () => {},
      onPickState: () => {},
      onShowAll: () => {},
      renderWorker: (session) => createElement("div", {
        key: session.id.toString(),
        className: "sb-session-row sb-child",
        "data-session": session.id.toString(),
      }),
      ...overrides,
    }));

  it("meters the workers by state and leads the brief summary with what needs the user", () => {
    const markup = render();

    expect(markup).toContain('aria-label="2 running, 2 blocked, 1 ready, 1 failed"');
    expect(markup).toContain('class="m-needs-input"');
    expect(markup).toContain("need you");
    expect(markup).toContain("6 workers");
    expect(markup).toContain('aria-expanded="true"');
  });

  it("marks the ready tally fresh while any counted child is a fresh idle", () => {
    const fresh = worker(5n, SessionState.IDLE);
    fresh.idleUnseen = true;
    const markup = render({ workers: [worker(1n, SessionState.WORKING), fresh] });
    expect(markup).toContain("c-idle is-fresh");

    const viewed = render({ workers: [worker(1n, SessionState.WORKING), worker(5n, SessionState.IDLE)] });
    expect(viewed).not.toContain("is-fresh");
  });

  it("keeps the per-state breakdown in the markup for the wide container query", () => {
    const markup = render();
    const full = markup.slice(markup.indexOf('class="sb-rollup-summary-full"'));

    expect(full).toContain("running");
    expect(full).toContain("blocked");
    expect(full).toContain("ready");
    expect(full).toContain("failed");
  });

  const briefOf = (markup: string) => markup.slice(
    markup.indexOf('class="sb-rollup-summary-brief"'),
    markup.indexOf('class="sb-rollup-summary-full"'),
  );

  it("summarizes a supervisor whose workers all want nothing", () => {
    const brief = briefOf(render({ workers: [worker(1n, SessionState.IDLE)] }));

    expect(brief).toContain("1 worker, all quiet");
    expect(brief).not.toContain("sb-rollup-tally");
  });

  it("leads with the running workers rather than calling them quiet", () => {
    const brief = briefOf(render({
      workers: [
        worker(1n, SessionState.WORKING),
        worker(2n, SessionState.STARTING),
        worker(3n, SessionState.IDLE),
      ],
    }));

    expect(brief).toContain("<b>2</b> running");
    expect(brief).toContain("3 workers");
    expect(brief).not.toContain("all quiet");
  });

  it("still leads with what needs the user while workers are running", () => {
    const brief = briefOf(render({
      workers: [worker(1n, SessionState.WORKING), worker(2n, SessionState.NEEDS_INPUT)],
    }));

    expect(brief).toContain("need you");
    expect(brief).not.toContain("running");
  });

  it("shows no worker rows at all while collapsed", () => {
    const markup = render({ collapsed: true });

    expect(markup).not.toContain("sb-children");
    expect(markup).not.toContain("data-session");
    expect(markup).toContain('aria-expanded="false"');
    expect(markup).toContain('aria-label="show the 6 workers under supervisor 2"');
  });

  it("opens filtered to one state, marks that tally active, and offers the way back", () => {
    const markup = render({ filter: "needs-input" });

    expect(markup).toContain('data-session="3"');
    expect(markup).toContain('data-session="4"');
    expect(markup).not.toContain('data-session="1"');
    expect(markup).toContain('aria-pressed="true"');
    expect(markup).toContain("showing 2 of 6 · show all");
  });

  it("falls back to the full list once a filtered state has no workers left", () => {
    const markup = render({ workers: workers.filter((s) => s.state !== SessionState.FAILED), filter: "failed" });

    expect(markup).toContain('data-session="1"');
    expect(markup).not.toContain("show all");
    expect(markup).not.toContain('aria-pressed="true"');
  });

  it("adds nothing under a supervisor that has spawned nothing", () => {
    const markup = render({ workers: [] });

    expect(markup).not.toContain("sb-supervisor-empty");
    expect(markup).not.toContain("sb-rollup");
    expect(markup).not.toContain("sb-children");
  });

  it("still says why the list is empty when a state filter is the reason", () => {
    const markup = render({ workers: [], emptyNote: "no failed workers" });

    expect(markup).toContain("no failed workers");
  });
});

describe("supervisorTree ordering", () => {
  const bucket = create(BucketSchema, { id: 1n, name: "work" });
  const active = <T extends { lastActivityAtUnixMs: bigint }>(s: T, ms: bigint): T => {
    s.lastActivityAtUnixMs = ms;
    return s;
  };
  // Newest first, which is all the sidebar's comparator does once ranks tie.
  const byRecency = (a: Session, b: Session) =>
    Number(b.lastActivityAtUnixMs - a.lastActivityAtUnixMs);
  const ids = (tree: ReturnType<typeof supervisorTree>) =>
    tree.entries.map((entry) =>
      entry.kind === "session" ? entry.session.id : entry.group.supervisor.id,
    );

  it("orders a loose session against a group rather than after every group", () => {
    // The reported bug: a session working now sat below a group nobody had
    // touched in an hour, because every group rendered before any loose row.
    const boss = active(supervisor(2n), 100n);
    const loose = active(worker(6n, SessionState.WORKING), 900n);
    const tree = supervisorTree(
      { bucket, supervisors: [boss], sessions: [loose] },
      byRecency,
    );
    expect(ids(tree)).toEqual([6n, 2n]);
  });

  it("lifts a group by its busiest member, not by the supervisor alone", () => {
    const boss = active(supervisor(2n), 100n);
    const spawned = active(worker(5n, SessionState.WORKING, { spawnedBy: 2n }), 900n);
    const loose = active(worker(6n, SessionState.WORKING), 500n);
    const tree = supervisorTree(
      { bucket, supervisors: [boss], sessions: [spawned, loose] },
      byRecency,
    );
    expect(ids(tree)).toEqual([2n, 6n]);
    expect(tree.entries[0].kind).toBe("group");
  });

  it("keeps a group's workers nested rather than promoting them to entries", () => {
    const boss = active(supervisor(2n), 100n);
    const spawned = active(worker(5n, SessionState.WORKING, { spawnedBy: 2n }), 900n);
    const tree = supervisorTree({ bucket, supervisors: [boss], sessions: [spawned] }, byRecency);
    expect(tree.entries).toHaveLength(1);
    expect(tree.loose).toEqual([]);
    expect(tree.supervisors[0].workers.map((w) => w.id)).toEqual([5n]);
  });
});

describe("BoardLink", () => {
  it("exposes active toggle semantics and the return action", () => {
    const markup = renderToStaticMarkup(createElement(BoardLink, {
      bucketId: "1",
      active: true,
      count: 2,
      onOpen: () => {},
    }));

    expect(markup).toContain('aria-pressed="true"');
    expect(markup).toContain('aria-label="board, active — return to previous view"');
    expect(markup).toContain('title="return to this bucket&#x27;s previous view"');
  });

  it("remains a native keyboard-activatable button when inactive", () => {
    const element = BoardLink({ bucketId: "1", active: false, count: 0, onOpen: () => {} }) as ReactElement<{ "aria-pressed": boolean }>;
    expect(element.type).toBe("button");
    expect(element.props["aria-pressed"]).toBe(false);
  });
});

describe("SessionGlance", () => {
  it("renders a PM item URL through the sidebar's actual glance renderer", () => {
    const markup = renderToStaticMarkup(
      createElement(SessionGlance, {
        fields: [
          create(ContextFieldSchema, {
            key: "item",
            label: "Item",
            kind: ContextKind.URL,
            value: "pm:item/1/33",
          }),
        ],
        sessionId: "47",
        onPmLink: () => {},
      }),
    );

    expect(markup).toContain('class="sb-session-chips"');
    expect(markup).toContain('href="#/bucket/1/item/33"');
    expect(markup).not.toContain('target="_blank"');
    expect(markup).not.toContain('href="pm:item/1/33"');
  });

  it("passes the source session id through the actual glance renderer", () => {
    const calls: Array<[unknown, string]> = [];
    const glance = SessionGlance({
      fields: [
        create(ContextFieldSchema, {
          key: "item",
          kind: ContextKind.URL,
          value: "pm:item/1/33",
        }),
      ],
      sessionId: "47",
      onPmLink: (link, sessionId) => calls.push([link, sessionId]),
    }) as ReactElement<{ children: ReactElement<{ onPmLink: (link: unknown) => void }>[] }>;

    glance.props.children[0].props.onPmLink({ kind: "item", bucketId: "1", id: "33" });

    expect(calls).toEqual([[{ kind: "item", bucketId: "1", id: "33" }, "47"]]);
  });
});

describe("SessionRoleIcon", () => {
  it.each([
    [SessionRole.WORKER, "Worker role", "is-worker"],
    [SessionRole.SUPERVISOR, "Supervisor role", "is-supervisor"],
  ])("renders an accessible compact role icon for %s", (role, label, className) => {
    const markup = renderToStaticMarkup(createElement(SessionRoleIcon, { role }));

    expect(markup).toContain(`class="sb-session-role ${className}"`);
    expect(markup).toContain(`aria-label="${label}`);
    expect(markup).toContain('tabindex="0"');
    expect(markup).toContain('role="tooltip"');
    expect(markup).not.toContain("data-tooltip=");
    expect(markup).not.toContain("title=");
    expect(markup).not.toContain(">Worker<");
    expect(markup).not.toContain(">Supervisor<");
  });
});

describe("SessionBranch", () => {
  it("renders nothing when the agent has reported no branch", () => {
    expect(renderToStaticMarkup(createElement(SessionBranch, { git: undefined }))).toBe("");
    expect(
      renderToStaticMarkup(
        createElement(SessionBranch, { git: create(SessionGitSchema, { worktree: "/repo" }) }),
      ),
    ).toBe("");
  });

  it("shows the branch and titles it plainly in the main checkout", () => {
    const git = create(SessionGitSchema, {
      branch: "pm/session-branch",
      worktree: "/repo",
      repoRoot: "/repo",
    });

    const markup = renderToStaticMarkup(createElement(SessionBranch, { git }));

    expect(markup).toContain("pm/session-branch");
    expect(markup).toContain('title="Branch pm/session-branch"');
  });

  it("names the worktree when the branch is checked out in a linked one", () => {
    const git = create(SessionGitSchema, {
      branch: "pm/session-branch",
      worktree: "/repo/.worktrees/session-branch",
      repoRoot: "/repo",
    });

    const markup = renderToStaticMarkup(createElement(SessionBranch, { git }));

    expect(markup).toContain(
      'title="pm/session-branch in worktree /repo/.worktrees/session-branch"',
    );
    expect(markup).toContain("sb-session-branch-wt");
  });
});

describe("visibleGlanceFields", () => {
  const field = (key: string, value: string) =>
    create(ContextFieldSchema, { key, label: key, value, kind: ContextKind.TEXT });

  it("drops fields whose key or value is blank so no empty chip renders", () => {
    const kept = visibleGlanceFields([
      field("", ""),
      field("tests", "passing"),
      field("status", "   "),
      field("  ", "orphan"),
    ]);
    expect(kept.map((f) => f.key)).toEqual(["tests"]);
  });

  it("keeps a fully blank bag empty so the chips line collapses", () => {
    expect(visibleGlanceFields([field("", "")])).toEqual([]);
  });
});

describe("filterSidebarByState", () => {
  const bucket = create(BucketSchema, { id: 1n, name: "work" });
  const other = create(BucketSchema, { id: 2n, name: "spare" });

  const model = (buckets: SidebarBucket[], orphans = [] as ReturnType<typeof worker>[]): SidebarModel =>
    ({ buckets, orphans, order: byId });

  it("keeps a supervisor that still owns a matching worker", () => {
    const boss = supervisor(2n);
    const blocked = worker(5n, SessionState.NEEDS_INPUT, { spawnedBy: 2n });
    const busy = worker(6n, SessionState.WORKING, { spawnedBy: 2n });

    const filtered = filterSidebarByState(
      model([{ bucket, supervisors: [boss], sessions: [blocked, busy] }]),
      "needs-input",
    );

    expect(filtered.buckets[0].supervisors.map((s) => s.id)).toEqual([2n]);
    expect(filtered.buckets[0].sessions.map((s) => s.id)).toEqual([5n]);
  });

  it("keeps a supervisor that matches on its own account", () => {
    const boss = create(SessionSchema, {
      id: 2n,
      projectId: 10n,
      role: SessionRole.SUPERVISOR,
      state: SessionState.NEEDS_INPUT,
    });

    const filtered = filterSidebarByState(
      model([{ bucket, supervisors: [boss], sessions: [worker(6n, SessionState.WORKING, { spawnedBy: 2n })] }]),
      "needs-input",
    );

    expect(filtered.buckets[0].supervisors.map((s) => s.id)).toEqual([2n]);
    expect(filtered.buckets[0].sessions).toEqual([]);
  });

  it("drops a supervisor whose workers all fell out of the filter", () => {
    const boss = supervisor(2n);

    const filtered = filterSidebarByState(
      model([{ bucket, supervisors: [boss], sessions: [worker(6n, SessionState.WORKING, { spawnedBy: 2n })] }]),
      "failed",
    );

    expect(filtered.buckets).toEqual([]);
  });

  it("drops buckets that keep nothing and filters loose sessions and orphans", () => {
    const kept = worker(5n, SessionState.FAILED);
    const filtered = filterSidebarByState(
      model(
        [
          { bucket, supervisors: [], sessions: [kept, worker(6n, SessionState.WORKING)] },
          { bucket: other, supervisors: [], sessions: [worker(7n, SessionState.IDLE)] },
        ],
        [worker(8n, SessionState.FAILED), worker(9n, SessionState.IDLE)],
      ),
      "failed",
    );

    expect(filtered.buckets.map((b) => b.bucket.id)).toEqual([1n]);
    expect(filtered.buckets[0].sessions.map((s) => s.id)).toEqual([5n]);
    expect(filtered.orphans.map((s) => s.id)).toEqual([8n]);
  });
});

describe("StatusTallies", () => {
  const render = (sessions: ReturnType<typeof worker>[], active: RollupState | null = null) =>
    renderToStaticMarkup(createElement(StatusTallies, { sessions, active, onPick: () => {} }));

  it("orders by attention and shows only the states that are present", () => {
    const markup = render([
      worker(5n, SessionState.IDLE),
      worker(6n, SessionState.WORKING),
      worker(7n, SessionState.NEEDS_INPUT),
      worker(8n, SessionState.NEEDS_INPUT),
    ]);

    const order = [...markup.matchAll(/sb-rollup-tally c-([a-z-]+)/g)].map((match) => match[1]);
    expect(order).toEqual(["needs-input", "working", "idle"]);
    expect(markup).toContain("<b>2</b> blocked");
    expect(markup).toContain("<b>1</b> running");
    expect(markup).not.toContain("c-failed");
  });

  it("stays quiet when nothing is running", () => {
    expect(render([])).toBe("");
    expect(render([worker(5n, SessionState.EXITED)])).toBe("");
  });

  it("marks the active tally pressed and accents an unseen finish", () => {
    const fresh = worker(5n, SessionState.IDLE);
    fresh.idleUnseen = true;
    const markup = render([fresh, worker(6n, SessionState.FAILED)], "failed");

    expect(markup).toContain('c-idle is-fresh');
    expect(markup).toMatch(/c-failed[^"]*is-active"[^>]*aria-pressed="true"/);
    expect(markup).toContain('title="show every session again"');
  });
});
