import { describe, expect, it } from "vitest";
import {
  approvalRoutePath,
  connectionRoutePath,
  parseRoute,
  reviewRoutePath,
  SETTINGS_GROUPS,
  SETTINGS_SECTIONS,
  settingsRoutePath,
  selectedSessionId,
  sessionHomeId,
  sessionRoutePath,
  withRememberedTab,
} from "./router";

describe("workspace routes", () => {
  it("routes daemon-persisted workspaces by numeric id", () => {
    expect(parseRoute("#/workspace/42")).toEqual({ name: "workspace", id: 42 });
  });

  it("does not treat a workspace as a selected session", () => {
    expect(selectedSessionId(parseRoute("#/workspace/42"))).toBeNull();
  });

  it("keeps the current session home while a workspace is active", () => {
    expect(sessionHomeId(parseRoute("#/session/7"), "3")).toBe("7");
    expect(sessionHomeId(parseRoute("#/workspace/42"), "7")).toBe("7");
  });
});

describe("board routes", () => {
  it("routes a bucket's board and a focused item", () => {
    expect(parseRoute("#/bucket/3/board")).toEqual({ name: "board", bucketId: "3" });
    expect(parseRoute("#/bucket/3/item/12")).toEqual({ name: "item", bucketId: "3", id: "12" });
    expect(parseRoute("#/item/33")).toEqual({ name: "legacy-item", legacyId: "33" });
    expect(parseRoute("#/bucket/3/item/900719925474099312345")).toEqual({
      name: "item",
      bucketId: "3",
      id: "900719925474099312345",
    });
  });

  it("keeps the session home while a board is active", () => {
    expect(sessionHomeId(parseRoute("#/bucket/3/board"), "7")).toBe("7");
    expect(selectedSessionId(parseRoute("#/bucket/3/item/12"))).toBeNull();
  });

  it("falls back home on malformed board paths", () => {
    expect(parseRoute("#/bucket/abc/board")).toEqual({ name: "home" });
    expect(parseRoute("#/item/abc")).toEqual({ name: "home" });
  });
});

describe("Settings routes", () => {
  it("routes every page at its one canonical address", () => {
    for (const section of SETTINGS_SECTIONS) {
      expect(parseRoute(`#/settings/${section}`)).toEqual({ name: "settings", section });
      expect(settingsRoutePath(section)).toBe(`/settings/${section}`);
    }
  });

  it("groups the account, workspace and controller pages in nav order", () => {
    expect(SETTINGS_GROUPS.map((group) => group.label)).toEqual([
      "Your account",
      "Workspace",
      "Controller",
    ]);
    expect(SETTINGS_SECTIONS).toEqual([
      "appearance", "terminal-theme", "notifications", "password",
      "projects", "connections", "models", "instructions",
      "workers", "mobile", "daemon",
    ]);
  });

  it("opens Appearance for a bare or unknown settings address", () => {
    expect(parseRoute("#/settings")).toEqual({ name: "settings", section: "appearance" });
    expect(parseRoute("#/settings/unknown")).toEqual({ name: "settings", section: "appearance" });
    expect(parseRoute("#/settings/a/b")).toEqual({ name: "settings", section: "appearance" });
  });

  it("is not a session route, and keeps the session to return to", () => {
    expect(selectedSessionId(parseRoute("#/settings"))).toBeNull();
    expect(sessionHomeId(parseRoute("#/settings"), "7")).toBe("7");
  });

  it("retains targeted bucket and project settings across direct routes", () => {
    expect(parseRoute("#/settings/projects?bucket=3")).toEqual({
      name: "settings",
      section: "projects",
      bucketId: "3",
    });
    expect(parseRoute("#/settings/projects?bucket=3&project=12")).toEqual({
      name: "settings",
      section: "projects",
      bucketId: "3",
      projectId: "12",
    });
    expect(parseRoute("#/settings/projects?bucket=oops&project=also-bad")).toEqual({
      name: "settings",
      section: "projects",
    });
  });

  it("preserves explicit bucket and project catalog modes through refresh", () => {
    expect(parseRoute("#/settings/projects?catalog=buckets")).toEqual({
      name: "settings",
      section: "projects",
      catalog: "buckets",
    });
    expect(parseRoute("#/settings/projects?catalog=projects")).toEqual({
      name: "settings",
      section: "projects",
      catalog: "projects",
    });
    expect(parseRoute("#/settings/projects?catalog=unknown")).toEqual({
      name: "settings",
      section: "projects",
    });
  });

  it("writes a Projects target into the address and drops it for other pages", () => {
    expect(settingsRoutePath("projects", { bucketId: "3" })).toBe("/settings/projects?bucket=3");
    expect(settingsRoutePath("projects", { bucketId: "3", projectId: "12" }))
      .toBe("/settings/projects?bucket=3&project=12");
    expect(settingsRoutePath("projects", { catalog: "buckets" }))
      .toBe("/settings/projects?catalog=buckets");
    expect(settingsRoutePath("workers", { bucketId: "3" })).toBe("/settings/workers");
  });
});

describe("old Manage addresses", () => {
  const canonical = (hash: string) => {
    const route = parseRoute(hash);
    return route.name === "settings" ? settingsRoutePath(route.section, route) : null;
  };

  it("lands every old Manage section on the same Settings page", () => {
    for (const section of ["projects", "connections", "models", "workers", "mobile", "instructions", "daemon"]) {
      expect(canonical(`#/manage/${section}`)).toBe(`/settings/${section}`);
    }
  });

  it("keeps Projects as the default for a bare or unknown Manage address", () => {
    expect(canonical("#/manage")).toBe("/settings/projects");
    expect(canonical("#/manage/unknown")).toBe("/settings/projects");
  });

  it("carries a bucket, project or catalog target across the redirect", () => {
    expect(canonical("#/manage/projects?bucket=3&project=12"))
      .toBe("/settings/projects?bucket=3&project=12");
    expect(canonical("#/manage/projects?catalog=buckets"))
      .toBe("/settings/projects?catalog=buckets");
  });
});

describe("a place inside a session", () => {
  it("keeps the tab and full-screen state in the URL", () => {
    expect(parseRoute("#/session/7")).toEqual({ name: "session", id: "7" });
    expect(parseRoute("#/session/7?tab=review:3")).toEqual({
      name: "session",
      id: "7",
      tab: "review:3",
    });
    expect(parseRoute("#/session/7?tab=9&focus=1")).toEqual({
      name: "session",
      id: "7",
      tab: "9",
      focus: true,
    });
  });

  it("leaves the default tab out of the address", () => {
    expect(sessionRoutePath("7")).toBe("/session/7");
    expect(sessionRoutePath("7", "agent")).toBe("/session/7");
    expect(sessionRoutePath("7", "review:3")).toBe("/session/7?tab=review%3A3");
    expect(sessionRoutePath("7", "agent", true)).toBe("/session/7?focus=1");
  });

  it("round-trips, so back and refresh land where you were", () => {
    for (const [tab, focus] of [
      ["agent", false],
      ["9", false],
      ["review:3", true],
    ] as const) {
      const route = parseRoute(`#${sessionRoutePath("7", tab, focus)}`);
      expect(route.name).toBe("session");
      if (route.name !== "session") continue;
      expect(route.tab ?? "agent").toBe(tab);
      expect(Boolean(route.focus)).toBe(focus);
    }
  });
});

describe("returning to a session", () => {
  it("carries the tab so a return lands where you left", () => {
    // The Board toggle navigates back without naming a tab, so the
    // caller supplies the remembered one rather than the pane
    // second-guessing an address that deliberately omitted it.
    expect(sessionRoutePath("7", "9")).toBe("/session/7?tab=9");
    expect(sessionRoutePath("7", undefined)).toBe("/session/7");
  });

  it("means a bare address really is the agent, so Back works", () => {
    // With no fallback in the pane, going back from ?tab=review:3 to a
    // bare session address must read as the agent and not as the tab
    // that was showing a moment ago.
    const back = parseRoute("#/session/7");
    expect(back.name).toBe("session");
    if (back.name === "session") expect(back.tab).toBeUndefined();
  });
});

describe("withRememberedTab", () => {
  it("restores the tab on a bare session address", () => {
    // Returning from the Board builds /session/7 with no tab, which
    // would otherwise land on the agent instead of the shell you left.
    expect(withRememberedTab("/session/7", { "7": "9" })).toBe("/session/7?tab=9");
  });

  it("leaves an address that already names a tab alone", () => {
    // Otherwise the URL would stop being authoritative and Back would
    // never change the view.
    expect(withRememberedTab("/session/7?tab=review:3", { "7": "9" })).toBe(
      "/session/7?tab=review:3",
    );
  });

  it("leaves anything that is not a session alone", () => {
    expect(withRememberedTab("/bucket/2/board", { "7": "9" })).toBe("/bucket/2/board");
    expect(withRememberedTab("/", {})).toBe("/");
  });

  it("returns a bare address when nothing is remembered", () => {
    expect(withRememberedTab("/session/7", {})).toBe("/session/7");
  });
});

describe("review routes", () => {
  it("routes a bare review, which means the reader's stored position", () => {
    expect(parseRoute("#/review/12")).toEqual({ name: "review", id: 12 });
  });

  it("carries the revision, file and comment a link names", () => {
    expect(parseRoute("#/review/12?view=round%3A3&file=src%2Flib.rs&thread=42")).toEqual({
      name: "review",
      id: 12,
      view: "round:3",
      file: "src/lib.rs",
      thread: 42,
    });
  });

  it("leaves out what the link does not name, rather than inventing a default", () => {
    expect(parseRoute("#/review/12?view=cum%3A2")).toEqual({
      name: "review",
      id: 12,
      view: "cum:2",
    });
  });

  it("ignores a thread that is not a positive whole number", () => {
    for (const bad of ["0", "-3", "x", "1.5", ""]) {
      expect(parseRoute(`#/review/12?thread=${bad}`)).toEqual({ name: "review", id: 12 });
    }
  });

  it("writes the live working tree as a bare path, since it is the default", () => {
    expect(reviewRoutePath(12)).toBe("/review/12");
    expect(reviewRoutePath(12, { view: "" })).toBe("/review/12");
  });

  it("round-trips every position it can write", () => {
    for (const at of [
      { view: "round:3" },
      { view: "cum:2", file: "src/lib.rs" },
      { view: "delta:4", file: "a b/c.ts", thread: 42 },
      { file: "src/lib.rs", thread: 7 },
    ]) {
      expect(parseRoute(`#${reviewRoutePath(12, at)}`)).toEqual({
        name: "review",
        id: 12,
        ...at,
      });
    }
  });

  it("does not treat a review as a selected session", () => {
    expect(selectedSessionId(parseRoute("#/review/12?view=round:3"))).toBeNull();
  });
});

describe("approval routes", () => {
  it("opens the list and one exact approval over it", () => {
    expect(parseRoute("#/approvals")).toEqual({ name: "approvals" });
    const id = "a".repeat(64);
    expect(parseRoute(`#${approvalRoutePath(id)}`)).toEqual({ name: "approvals", id });
    expect(approvalRoutePath()).toBe("/approvals");
  });

  it("opens the list alone for an id that is not an approval token", () => {
    expect(parseRoute("#/approvals/")).toEqual({ name: "approvals" });
    expect(parseRoute("#/approvals/%2e%2e")).toEqual({ name: "approvals" });
    expect(parseRoute("#/approvals/x/y")).toEqual({ name: "home" });
  });

  it("is not a session route", () => {
    expect(selectedSessionId(parseRoute("#/approvals/abc"))).toBeNull();
  });
});

describe("places inside the Connections page", () => {
  const callId = "e3c946336a9d8063";

  it("round-trips a connection, its setup, a new connection and a call", () => {
    for (const connection of [
      { view: "detail", id: 1 },
      { view: "setup", id: 2 },
      { view: "new" },
      { view: "call", callId },
    ] as const) {
      expect(parseRoute(`#${connectionRoutePath(connection)}`)).toEqual({
        name: "settings",
        section: "connections",
        connection,
      });
      expect(settingsRoutePath("connections", { connection })).toBe(connectionRoutePath(connection));
    }
    expect(connectionRoutePath()).toBe("/settings/connections");
    expect(settingsRoutePath("connections")).toBe("/settings/connections");
    expect(connectionRoutePath({ view: "setup", id: 2 })).toBe("/settings/connections/2/setup");
    expect(connectionRoutePath({ view: "call", callId })).toBe(`/settings/connections/calls/${callId}`);
  });

  it("reads the same places from the Manage addresses they once had", () => {
    expect(parseRoute("#/manage/connections")).toEqual({ name: "settings", section: "connections" });
    expect(parseRoute("#/manage/connections/new")).toEqual({
      name: "settings", section: "connections", connection: { view: "new" },
    });
    expect(parseRoute("#/manage/connections/3")).toEqual({
      name: "settings", section: "connections", connection: { view: "detail", id: 3 },
    });
    expect(parseRoute("#/manage/connections/3/setup")).toEqual({
      name: "settings", section: "connections", connection: { view: "setup", id: 3 },
    });
    expect(parseRoute(`#/manage/connections/calls/${callId}`)).toEqual({
      name: "settings", section: "connections", connection: { view: "call", callId },
    });
  });

  it("opens the list for an address that names nothing known", () => {
    for (const hash of [
      "#/settings/connections/0",
      "#/settings/connections/abc",
      "#/settings/connections/1/policy",
      "#/settings/connections/calls/not%20an%20id",
      "#/settings/connections/calls/",
      "#/manage/connections/abc",
    ]) {
      expect(parseRoute(hash)).toEqual({ name: "settings", section: "connections" });
    }
  });

  it("carries no connection target onto another page", () => {
    expect(settingsRoutePath("workers", { connection: { view: "new" } })).toBe("/settings/workers");
  });
});
