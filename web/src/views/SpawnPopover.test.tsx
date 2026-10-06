import { create } from "@bufbuild/protobuf";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  AgentDialectsSchema,
  AgentKind,
  BucketSchema,
  ConnectMode,
  ModelDialect,
  ModelProfileEndpointSchema,
  ModelProfileSchema,
  PermissionMode,
  ProjectSchema,
  SessionRole,
  WorkerSchema,
} from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { AGENTS } from "@puppet-master/client-core/state/agent";
import { initialState, type AppState } from "@puppet-master/client-core/state/reducer";
import type { PmClient } from "@puppet-master/client-core/ws/client";
import { ClientContext } from "../state/hooks";
import {
  chipMenuShift,
  escapeClosesMenuFirst,
  spawnPopoverPosition,
  SpawnPopover,
  SpawnPopoverPanel,
  type SpawnDraft,
  type SpawnChipKey,
  type SpawnPopoverTarget,
} from "./SpawnPopover";

const bucket = create(BucketSchema, {
  id: 1n,
  name: "BM",
  permissionMode: PermissionMode.DEFAULT,
});
const trucks = create(ProjectSchema, {
  id: 4n,
  bucketId: 1n,
  name: "trucks",
  path: "/srv/trucks",
  allowedWorkerIds: [0n, 7n],
});
const configurator = create(ProjectSchema, {
  id: 2n,
  bucketId: 1n,
  name: "configurator",
  path: "/srv/configurator",
  allowedWorkerIds: [0n, 7n],
});
const localWorker = create(WorkerSchema, { id: 0n, name: "lima", online: true });
const remoteWorker = create(WorkerSchema, { id: 7n, name: "mac-vm", online: false });
const gateway = create(ModelProfileSchema, {
  id: 5n,
  name: "Gateway",
  endpoints: [
    create(ModelProfileEndpointSchema, {
      profileId: 5n,
      dialect: ModelDialect.ANTHROPIC_MESSAGES,
      model: "gateway/big",
    }),
  ],
});
const agentDialects = [
  create(AgentDialectsSchema, {
    agent: AgentKind.CLAUDE_CODE,
    dialects: [ModelDialect.ANTHROPIC_MESSAGES],
  }),
  create(AgentDialectsSchema, {
    agent: AgentKind.CODEX,
    dialects: [ModelDialect.OPENAI_RESPONSES],
  }),
];

function fixtureState(overrides: Partial<AppState> = {}): AppState {
  return {
    ...initialState,
    buckets: new Map([["1", bucket]]),
    projects: new Map([
      ["4", trucks],
      ["2", configurator],
    ]),
    workers: new Map([
      ["0", localWorker],
      ["7", remoteWorker],
    ]),
    ...overrides,
  };
}

function fakeClient(state: AppState): PmClient {
  return {
    subscribe: () => () => {},
    getState: () => state,
  } as unknown as PmClient;
}

function renderPopover(target: SpawnPopoverTarget, state = fixtureState()): string {
  return renderToStaticMarkup(
    <ClientContext.Provider value={fakeClient(state)}>
      <SpawnPopover
        open
        onToggle={() => {}}
        onClose={() => {}}
        onCreated={() => {}}
        triggerTitle="spawn here"
        target={target}
      />
    </ClientContext.Provider>,
  );
}

const noop = () => {};

function renderPanel({
  target = { kind: "bucket", bucketId: "1" },
  state = fixtureState(),
  draft = {},
  defaultProjectId = "2",
  chipMenu = null,
  expanded = false,
  busy = false,
  error = null,
}: {
  target?: SpawnPopoverTarget;
  state?: AppState;
  draft?: Partial<SpawnDraft>;
  defaultProjectId?: string;
  chipMenu?: SpawnChipKey | null;
  expanded?: boolean;
  busy?: boolean;
  error?: string | null;
} = {}): string {
  const fullDraft: SpawnDraft = {
    role: SessionRole.WORKER,
    projectId: "2",
    workerId: 0n,
    agentOverride: undefined,
    profileOverride: "",
    permMode: PermissionMode.UNSPECIFIED,
    title: "",
    prompt: "",
    itemsApi: true,
    cwd: "/srv/configurator",
    ...draft,
  };
  return renderToStaticMarkup(
    <SpawnPopoverPanel
      target={target}
      state={state}
      draft={fullDraft}
      onPickRole={noop}
      defaultProjectId={defaultProjectId}
      chipMenu={chipMenu}
      expanded={expanded}
      busy={busy}
      error={error}
      onToggleChipMenu={noop}
      onPickProject={noop}
      onPickWorker={noop}
      onPickAgent={noop}
      onSetProfile={noop}
      onSetPermMode={noop}
      onSetTitle={noop}
      onSetPrompt={noop}
      onSetItemsApi={noop}
      onEditCwd={noop}
      onToggleExpanded={noop}
      onDismiss={noop}
      onSubmit={noop}
    />,
  );
}

describe("SpawnPopover variants", () => {
  it("opens the bucket ＋ as a supervisor sentence with quiet default chips", () => {
    const html = renderPopover({ kind: "bucket", bucketId: "1" });
    // Supervisor is the default, so the role chip reads as one and stays
    // quiet like every other inherited default.
    expect(html).toMatch(/data-chip="role"[^>]*>supervisor</);
    expect(html).toMatch(/is-default[^>]*data-chip="role"/);
    // The default home is the bucket's first project by name, rendered quiet.
    expect(html).toMatch(/data-chip="project"[^>]*>configurator</);
    expect(html).toMatch(/is-default[^>]*data-chip="project"/);
    expect(html).toMatch(/data-chip="host"[^>]*>lima</);
    expect(html).toMatch(/is-default[^>]*data-chip="host"/);
    expect(html).toMatch(/data-chip="agent"[^>]*>Claude</);
    expect(html).toMatch(/is-default[^>]*data-chip="agent"/);
    expect(html).toContain('aria-label="dismiss"');
    expect(html).toContain("▾ permissions, profile, directory");
    expect(html).toContain('type="submit"');
    expect(html).toContain('aria-expanded="true"');
    expect(html).not.toContain("spawn-items-api");
  });

  it("offers the supervisor role on the same bucket ＋, rather than a second button", () => {
    const html = renderPanel({
      target: { kind: "bucket", bucketId: "1" },
      draft: { role: SessionRole.SUPERVISOR },
      chipMenu: "role",
    });
    expect(html).toMatch(/data-chip="role"[^>]*>supervisor</);
    expect(html).toContain(">worker<");
  });
});

describe("chipMenuShift", () => {
  const width = 1000;

  it("leaves a menu that already fits exactly where it opened", () => {
    expect(chipMenuShift({ left: 100, right: 400 }, width)).toBe(0);
  });

  it("pulls a menu back only as far as its overflow, not to an edge", () => {
    // Overflows the right margin by 20, so it moves 20 and no further.
    expect(chipMenuShift({ left: 700, right: 1012 }, width)).toBe(-20);
  });

  it("pushes a menu that opened off the left back into view", () => {
    expect(chipMenuShift({ left: -30, right: 200 }, width)).toBe(38);
  });

  it("keeps the left edge visible rather than chasing the right one", () => {
    // Wider than the window: rescuing the right edge entirely would push
    // the options that matter off the left, so the left margin wins and
    // the menu settles against it however far the right still overflows.
    expect(chipMenuShift({ left: 4, right: 1200 }, width)).toBe(4);
    expect(chipMenuShift({ left: 40, right: 1200 }, width)).toBe(-32);
  });

  it("rescues a menu hanging off both edges by the left one", () => {
    // The case the browser found: a wide menu overflowing right must not
    // have its left overflow ignored because the right was handled first.
    expect(chipMenuShift({ left: -2, right: 1100 }, width)).toBe(10);
  });
});

describe("SpawnPopover chips", () => {
  it("gives every chip menu the same anchor and lets measurement place it", () => {
    // A hand-picked edge cannot be right for every chip: the chips sit
    // inline in a sentence that reflows, so the same edge that saves the
    // last chip sends the first one off the other side.
    for (const chipMenu of ["agent", "role", "project", "host"] as const) {
      const html = renderPanel({ chipMenu });
      expect(html).toContain("popover-menu-left");
      expect(html).not.toContain("popover-menu-right");
    }
  });

  it("accents every chip that differs from its inherited default", () => {
    const html = renderPanel({
      draft: {
        // Worker is the departure from the default now, so it is the role
        // that should be accented rather than left quiet.
        role: SessionRole.WORKER,
        projectId: "4",
        workerId: 7n,
        agentOverride: AgentKind.CODEX,
        cwd: "/srv/trucks",
      },
    });
    expect(html).toMatch(/data-chip="project"[^>]*>trucks</);
    expect(html).toMatch(/data-chip="host"[^>]*>mac-vm</);
    expect(html).toMatch(/data-chip="agent"[^>]*>Codex</);
    expect(html).not.toContain("is-default");
  });

  it("opens one chip menu with the active choice marked", () => {
    const html = renderPanel({ chipMenu: "agent" });
    expect(html).toContain('role="menu"');
    expect(html).toContain("Claude — fallback default");
    expect(html).toMatch(/aria-checked="true"[^>]*>[^<]*<span[^>]*>●<\/span>Claude — fallback default/);
    // The rows come from the shared agent table, so every agent a spawn
    // can choose is offered next to the inherited default.
    for (const agent of AGENTS) {
      expect(html).toMatch(
        new RegExp(`aria-checked="false"[^>]*>[^<]*<span[^>]*>○</span>${agent.label}<`),
      );
    }
  });

  it("lists the bucket's projects in the project chip menu", () => {
    const html = renderPanel({ chipMenu: "project" });
    expect(html).toContain(">configurator<");
    expect(html).toContain(">trucks<");
  });

  it("marks host menu entries with locality and availability", () => {
    const html = renderPanel({ chipMenu: "host" });
    expect(html).toContain("lima (local)");
    expect(html).toContain("mac-vm — offline");
  });
});

describe("SpawnPopover fold", () => {
  it("expands in place to the rarely needed fields, including the worker items-API checkbox", () => {
    const html = renderPanel({
      target: { kind: "bucket", bucketId: "1" },
      draft: { projectId: "4", cwd: "/srv/trucks" },
      state: fixtureState({ modelProfiles: new Map([["5", gateway]]), agentDialects }),
      expanded: true,
    });
    expect(html).toContain("▴ fewer options");
    expect(html).toContain("permission mode");
    expect(html).toContain("model profile");
    expect(html).toContain("working directory");
    expect(html).toContain("title (optional)");
    expect(html).toContain("starting prompt (optional)");
    expect(html).toContain("rarely needed — you brief the agent in its terminal");
    expect(html).toContain("spawn-items-api");
    expect(html).toContain("Items API — let this Worker file and update board items");
  });

  it("offers no items-API checkbox for a supervisor, whose APIs are fixed", () => {
    const html = renderPanel({ expanded: true, draft: { role: SessionRole.SUPERVISOR } });
    expect(html).toContain("permission mode");
    expect(html).not.toContain("spawn-items-api");
  });
});

describe("SpawnPopover validation", () => {
  it("warns that a host which dials the controller never did, and refuses to spawn", () => {
    const html = renderPanel({ draft: { workerId: 7n } });
    expect(html).toContain("worker 7 has not connected");
    expect(html).toMatch(/type="submit"[^>]*disabled/);
  });

  it("warns instead that the controller cannot reach a host it dials", () => {
    const dialed = create(WorkerSchema, {
      id: 7n,
      name: "mac-vm",
      online: false,
      connectMode: ConnectMode.ACCEPT,
      endpoint: "10.0.0.5:7677",
    });
    const html = renderPanel({
      draft: { workerId: 7n },
      state: fixtureState({ workers: new Map([["0", localWorker], ["7", dialed]]) }),
    });
    expect(html).toContain("worker 7 cannot be reached at 10.0.0.5:7677");
    expect(html).toMatch(/type="submit"[^>]*disabled/);
  });

  it("says when the effective profile has no endpoint the agent can use", () => {
    const html = renderPanel({
      draft: { agentOverride: AgentKind.CODEX, profileOverride: "5" },
      state: fixtureState({ modelProfiles: new Map([["5", gateway]]), agentDialects }),
    });
    expect(html).toContain("Gateway has no endpoint Codex can use — this spawn will be rejected");
  });

  it("shows a submit error inline", () => {
    const html = renderPanel({ error: "spawn failed" });
    expect(html).toContain("spawn failed");
  });
});

describe("SpawnPopover keyboard contract", () => {
  it("routes Escape to the open chip menu before the popover", () => {
    expect(escapeClosesMenuFirst("agent")).toBe("menu");
    expect(escapeClosesMenuFirst("host")).toBe("menu");
    expect(escapeClosesMenuFirst(null)).toBe("popover");
  });

  it("spawns on Enter as the form's submit action", () => {
    const html = renderPanel();
    // The surface is a form and its only submit control is the spawn
    // button; the form's own Enter handling covers text fields, so Enter
    // spawns unless a chip menu or another control consumes the key.
    expect(html).toMatch(/<form[^>]*class="spawn-pop"/);
    expect(html).toMatch(/type="submit"[^>]*>spawn/);
  });
});

describe("spawnPopoverPosition", () => {
  const viewport = { width: 1200, height: 800 };
  const panel = { width: 440, height: 320 };

  it("anchors at the trigger button and opens below it", () => {
    const trigger = { top: 100, bottom: 122, left: 200, right: 222 };
    const pos = spawnPopoverPosition(trigger, panel, viewport);
    expect(pos).toEqual({ left: 200, top: 126 });
  });

  it("pulls back from the right viewport margin when sidebar is wide", () => {
    const trigger = { top: 100, bottom: 122, left: 900, right: 922 };
    const pos = spawnPopoverPosition(trigger, panel, viewport);
    // 1200 - 440 - 12 = 748
    expect(pos).toEqual({ left: 748, top: 126 });
  });

  it("flips above the trigger when overflowing the bottom viewport edge", () => {
    // Trigger is near bottom (bottom: 650, top: 628). 654 + 320 = 974 > 800 - 12 (788).
    // Flips above: 628 - 320 - 4 = 304.
    const trigger = { top: 628, bottom: 650, left: 200, right: 222 };
    const pos = spawnPopoverPosition(trigger, panel, viewport);
    expect(pos).toEqual({ left: 200, top: 304 });
  });

  it("clamps to viewport margins on very short screens where neither fits", () => {
    const shortViewport = { width: 1200, height: 300 };
    const trigger = { top: 140, bottom: 162, left: 200, right: 222 };
    const pos = spawnPopoverPosition(trigger, panel, shortViewport);
    // Neither above (140 - 320 - 4 < 12) nor below fits. Clamps to max(12, 300 - 320 - 12) = 12.
    expect(pos.top).toBe(12);
    expect(pos.left).toBe(200);
  });
});

