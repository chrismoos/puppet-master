import { describe, expect, it } from "vitest";
import { sessionLaunchFolder, sessionLaunchMetadata } from "./sessionLaunch";

const project = { name: "puppet-master", path: "/work/puppet-master" };

describe("sessionLaunchFolder", () => {
  it("omits ordinary project-root launches", () => {
    expect(sessionLaunchFolder({ cwd: "/work/puppet-master/", workerId: 0n }, project)).toBeNull();
  });

  it("shows a concise project-relative nested folder with the full path explained", () => {
    expect(sessionLaunchFolder(
      { cwd: "/work/puppet-master/packages/équipe/非常に長いフォルダー", workerId: 0n },
      project,
    )).toEqual({
      label: "folder · packages/équipe/非常に長いフォルダー",
      description: expect.stringContaining("Launch folder: /work/puppet-master/packages/équipe/非常に長いフォルダー."),
    });
  });

  it("labels local paths outside the project without claiming git state", () => {
    const result = sessionLaunchFolder({ cwd: "/tmp/non-git", workerId: 0n }, project);
    expect(result?.label).toBe("folder · non-git");
    expect(result?.description).toContain("does not infer git status");
  });

  it("explains a distinct remote worker path", () => {
    const result = sessionLaunchFolder(
      { cwd: "/srv/worker/checkouts/puppet-master", workerId: 8n },
      project,
    );
    expect(result?.label).toBe("remote folder · puppet-master");
    expect(result?.description).toContain("Remote project mappings may use a different root");
  });

  it("handles Windows roots and comparisons", () => {
    expect(sessionLaunchFolder(
      { cwd: "c:\\WORK\\puppet-master\\", workerId: 0n },
      { name: "puppet-master", path: "C:\\work\\puppet-master" },
    )).toBeNull();
    expect(sessionLaunchFolder(
      { cwd: "C:\\work\\puppet-master\\tools", workerId: 0n },
      { name: "puppet-master", path: "C:\\work\\puppet-master" },
    )?.label).toBe("folder · tools");
  });

  it("makes missing and opaque legacy cwd values explicit", () => {
    expect(sessionLaunchFolder({ cwd: "", workerId: 0n }, project)?.label).toBe("folder unavailable");
    const opaque = sessionLaunchFolder({ cwd: "(*)", workerId: 0n }, project);
    expect(opaque?.label).toBe("folder unavailable");
    expect(opaque?.description).toContain("no repository-status meaning");
    expect(sessionLaunchFolder({ cwd: "/legacy/(*)", workerId: 0n }, project)?.label).toBe("folder unavailable");
  });

  it("handles a filesystem-root project without inventing metadata", () => {
    expect(sessionLaunchFolder(
      { cwd: "/", workerId: 0n },
      { name: "root", path: "/" },
    )).toBeNull();
    expect(sessionLaunchFolder(
      { cwd: "/nested", workerId: 0n },
      { name: "root", path: "/" },
    )?.label).toBe("folder · nested");
  });
});

describe("sessionLaunchMetadata", () => {
  it("keeps project-root and nested paths visible in detailed session info", () => {
    expect(sessionLaunchMetadata(
      { cwd: "/work/puppet-master", workerId: 0n },
      project,
    )).toEqual({
      origin: "local",
      path: "/work/puppet-master",
      pathKind: "project-root",
      pathContext: "Configured project root.",
    });

    expect(sessionLaunchMetadata(
      { cwd: "/work/puppet-master/packages/équipe/非常に長いフォルダー", workerId: 0n },
      project,
    )).toMatchObject({
      origin: "local",
      pathKind: "project-nested",
      pathContext: "Nested folder within the configured project: packages/équipe/非常に長いフォルダー",
    });
  });

  it("distinguishes remote, missing, deleted-project, and opaque legacy states", () => {
    expect(sessionLaunchMetadata(
      { cwd: "/srv/checkouts/repo", workerId: 88n },
      project,
    )).toMatchObject({ origin: "remote", pathKind: "recorded" });

    expect(sessionLaunchMetadata({ cwd: "", workerId: 0n }, project)).toMatchObject({
      path: null,
      pathKind: "missing",
      pathContext: expect.stringContaining("older sessions"),
    });

    expect(sessionLaunchMetadata(
      { cwd: "/deleted/project/path", workerId: 0n },
      undefined,
    )).toMatchObject({
      pathKind: "recorded",
      pathContext: expect.stringContaining("project is unavailable"),
    });

    expect(sessionLaunchMetadata({ cwd: "/legacy/(*)", workerId: 0n }, project)).toMatchObject({
      pathKind: "opaque",
      pathContext: expect.stringContaining("no repository-status meaning"),
    });
  });
});
