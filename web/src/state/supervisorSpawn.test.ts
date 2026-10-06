import { afterEach, describe, expect, it, vi } from "vitest";
import { SessionRole, type Project } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import {
  defaultSpawnProjectId,
  readRememberedSpawnProject,
  rememberSpawnProject,
} from "./supervisorSpawn";

const project = (id: number, bucketId: number, name: string) =>
  ({ id: BigInt(id), bucketId: BigInt(bucketId), name }) as Project;

afterEach(() => vi.unstubAllGlobals());

const stubStorage = () => {
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  });
};

describe("the spawn home project", () => {
  const projects = [
    project(4, 1, "trucks"),
    project(2, 1, "configurator"),
    project(9, 2, "lander"),
  ];

  it("defaults to the bucket's first project by name", () => {
    expect(defaultSpawnProjectId("1", projects, undefined)).toBe("2");
  });

  it("prefers the project last used for that role in the bucket", () => {
    expect(defaultSpawnProjectId("1", projects, "4")).toBe("4");
  });

  it("ignores a remembered project that left the bucket", () => {
    expect(defaultSpawnProjectId("1", projects, "9")).toBe("2");
    expect(defaultSpawnProjectId("1", projects, "77")).toBe("2");
  });

  it("returns nothing for a bucket with no projects", () => {
    expect(defaultSpawnProjectId("3", projects, "4")).toBe("");
  });

  it("remembers per bucket and round-trips through storage", () => {
    stubStorage();
    const W = SessionRole.WORKER;
    expect(readRememberedSpawnProject("1", W)).toBeUndefined();
    rememberSpawnProject("1", W, "4");
    rememberSpawnProject("2", W, "9");
    expect(readRememberedSpawnProject("1", W)).toBe("4");
    expect(readRememberedSpawnProject("2", W)).toBe("9");
    rememberSpawnProject("1", W, "2");
    expect(readRememberedSpawnProject("1", W)).toBe("2");
  });

  it("keeps each role's home apart, so picking one does not move the other", () => {
    stubStorage();
    rememberSpawnProject("1", SessionRole.WORKER, "4");
    rememberSpawnProject("1", SessionRole.SUPERVISOR, "2");
    expect(readRememberedSpawnProject("1", SessionRole.WORKER)).toBe("4");
    expect(readRememberedSpawnProject("1", SessionRole.SUPERVISOR)).toBe("2");
  });
});
