import { afterEach, describe, expect, it, vi } from "vitest";
import {
  AgentKind,
  ItemStatus,
  PermissionMode,
  SessionRole,
} from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import type { PmClient } from "@puppet-master/client-core/ws/client";
import { submitSpawn, type SpawnRequest } from "./spawnSubmit";
import { readRememberedSpawnProject } from "./supervisorSpawn";

afterEach(() => vi.unstubAllGlobals());

const stubStorage = () => {
  const values = new Map<string, string>();
  vi.stubGlobal("localStorage", {
    getItem: (key: string) => values.get(key) ?? null,
    setItem: (key: string, value: string) => values.set(key, value),
    removeItem: (key: string) => values.delete(key),
  });
};

function fakeClient(overrides: {
  createdId?: bigint;
  spawnRejects?: Error;
  upsertRejects?: Error;
} = {}) {
  const spawnCalls: unknown[][] = [];
  const upsertCalls: unknown[] = [];
  const client = {
    spawnSession: (...args: unknown[]) => {
      spawnCalls.push(args);
      if (overrides.spawnRejects) return Promise.reject(overrides.spawnRejects);
      return Promise.resolve({ createdId: overrides.createdId });
    },
    upsertItem: (write: unknown) => {
      upsertCalls.push(write);
      if (overrides.upsertRejects) return Promise.reject(overrides.upsertRejects);
      return Promise.resolve({});
    },
  } as unknown as PmClient;
  return { client, spawnCalls, upsertCalls };
}

const request = (overrides: Partial<SpawnRequest> = {}): SpawnRequest => ({
  projectId: "7",
  agent: AgentKind.CODEX,
  title: "  padded title  ",
  prompt: "the prompt",
  cwdOverride: "/explicit/dir",
  permissionMode: PermissionMode.AUTO,
  workerId: 3n,
  itemsApi: false,
  role: SessionRole.SUPERVISOR,
  modelProfileId: 5n,
  ...overrides,
});

describe("submitSpawn", () => {
  it("passes the spawn to the daemon in its exact argument order", async () => {
    stubStorage();
    const { client, spawnCalls } = fakeClient({ createdId: 42n });
    const createdId = await submitSpawn(client, request());
    expect(createdId).toBe(42n);
    expect(spawnCalls).toEqual([[
      7n,
      AgentKind.CODEX,
      "padded title",
      "the prompt",
      "/explicit/dir",
      PermissionMode.AUTO,
      3n,
      false,
      true,
      SessionRole.SUPERVISOR,
      5n,
      undefined,
      undefined,
    ]]);
  });

  it("passes initial cols and rows when provided in the request", async () => {
    stubStorage();
    const { client, spawnCalls } = fakeClient({ createdId: 42n });
    await submitSpawn(client, request({ initialCols: 140, initialRows: 40 }));
    expect(spawnCalls).toEqual([[
      7n,
      AgentKind.CODEX,
      "padded title",
      "the prompt",
      "/explicit/dir",
      PermissionMode.AUTO,
      3n,
      false,
      true,
      SessionRole.SUPERVISOR,
      5n,
      140,
      40,
    ]]);
  });

  it("remembers the home project only after a successful spawn", async () => {
    stubStorage();
    const { client } = fakeClient({ createdId: 42n });
    await submitSpawn(client, request({ spawnBucketId: "1" }));
    expect(readRememberedSpawnProject("1", SessionRole.SUPERVISOR)).toBe("7");
  });

  it("does not touch the remembered home when the daemon refuses the spawn", async () => {
    stubStorage();
    const { client } = fakeClient({ spawnRejects: new Error("refused") });
    await expect(submitSpawn(client, request({ spawnBucketId: "1" }))).rejects.toThrow("refused");
    expect(readRememberedSpawnProject("1", SessionRole.SUPERVISOR)).toBeUndefined();
  });

  it("marks a linked item in progress with the created session", async () => {
    stubStorage();
    const { client, upsertCalls } = fakeClient({ createdId: 42n });
    await submitSpawn(client, request({
      role: SessionRole.WORKER,
      item: { id: 9n, bucketId: 1n },
    }));
    expect(upsertCalls).toEqual([{
      bucketId: 1n,
      id: 9n,
      status: ItemStatus.IN_PROGRESS,
      linkSessionId: 42n,
    }]);
  });

  it("skips the item link when the daemon reports no created session", async () => {
    stubStorage();
    const { client, upsertCalls } = fakeClient();
    const createdId = await submitSpawn(client, request({ item: { id: 9n, bucketId: 1n } }));
    expect(createdId).toBeUndefined();
    expect(upsertCalls).toEqual([]);
  });

  it("treats the item link as best-effort once the spawn succeeded", async () => {
    stubStorage();
    const { client } = fakeClient({ createdId: 42n, upsertRejects: new Error("item gone") });
    await expect(submitSpawn(client, request({ item: { id: 9n, bucketId: 1n } }))).resolves.toBe(42n);
  });
});
