import { describe, expect, it } from "vitest";
import {
  defaultSpawnCwd,
  initializeSpawnCwd,
  spawnCwdReducer,
} from "./spawnCwd";

describe("spawn dialog working-directory defaults", () => {
  it("initializes a project (+) dialog with the configured local project path", () => {
    const defaultCwd = defaultSpawnCwd(
      { path: "/work/configured-project", workerPaths: [] },
      0n,
    );

    expect(initializeSpawnCwd(defaultCwd)).toEqual({
      value: "/work/configured-project",
      touched: false,
    });
  });

  it("ignores a legacy local mapping because local launch uses the configured path", () => {
    expect(defaultSpawnCwd({
      path: "/configured/local-project",
      workerPaths: [{ workerId: 0n, path: "/stale/local-mapping" }],
    }, 0n)).toBe("/configured/local-project");
  });

  it("uses an effective worker mapping remotely and the project path when unmapped", () => {
    const firstProject = {
      path: "/configured/project-one",
      workerPaths: [{ workerId: 7n, path: "/remote/project-one" }],
    };
    const secondProject = { path: "/configured/project-two", workerPaths: [] };
    const local = defaultSpawnCwd(firstProject, 0n);
    const remote = defaultSpawnCwd(firstProject, 7n);

    let cwd = initializeSpawnCwd(local);
    cwd = spawnCwdReducer(cwd, {
      type: "selection-changed",
      value: defaultSpawnCwd(secondProject, 7n),
    });
    expect(cwd).toEqual({ value: "/configured/project-two", touched: false });

    cwd = spawnCwdReducer(cwd, { type: "selection-changed", value: remote });
    expect(cwd).toEqual({ value: "/remote/project-one", touched: false });

    cwd = spawnCwdReducer(cwd, { type: "selection-changed", value: local });
    expect(cwd).toEqual({ value: "/configured/project-one", touched: false });
  });

  it("keeps intentional edits across background default updates and resets on an explicit switch", () => {
    let cwd = initializeSpawnCwd("/projects/one");
    cwd = spawnCwdReducer(cwd, { type: "default-changed", value: "/projects/one-updated" });
    expect(cwd).toEqual({ value: "/projects/one-updated", touched: false });

    cwd = spawnCwdReducer(cwd, { type: "edit", value: "/custom/checkout" });
    cwd = spawnCwdReducer(cwd, { type: "default-changed", value: "/projects/one-replaced" });
    expect(cwd).toEqual({ value: "/custom/checkout", touched: true });

    cwd = spawnCwdReducer(cwd, { type: "selection-changed", value: "/projects/two" });
    expect(cwd).toEqual({ value: "/projects/two", touched: false });
  });
});
