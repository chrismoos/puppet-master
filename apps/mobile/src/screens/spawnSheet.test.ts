import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import {
  ProjectSchema,
  BucketSchema,
  WorkerSchema,
  AgentKind,
  PermissionMode,
} from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { LOCAL_WORKER_ID } from "@puppet-master/client-core/format";
import { agentLabel, resolveAgent } from "@puppet-master/client-core/state/agent";
import { resolveModelProfile } from "@puppet-master/client-core/state/modelProfile";
import { resolveProjectWorkerId, workerUnavailableReason } from "@puppet-master/client-core/state/worker";

describe("spawn form defaults", () => {
  const bucket = create(BucketSchema, { id: 1n, name: "main", defaultWorkerId: 2n });
  const project = create(ProjectSchema, { id: 10n, bucketId: 1n, name: "api" });
  const projectWithWorker = create(ProjectSchema, { id: 11n, bucketId: 1n, name: "web", workerId: 3n });

  it("resolves worker from bucket when project has no override", () => {
    expect(resolveProjectWorkerId(project, bucket)).toBe(2n);
  });

  it("resolves worker from project override", () => {
    expect(resolveProjectWorkerId(projectWithWorker, bucket)).toBe(3n);
  });

  it("falls back to local worker when no overrides", () => {
    const bare = create(ProjectSchema, { id: 12n, bucketId: 1n, name: "bare" });
    const emptyBucket = create(BucketSchema, { id: 2n, name: "empty" });
    expect(resolveProjectWorkerId(bare, emptyBucket)).toBe(LOCAL_WORKER_ID);
  });
});

describe("worker availability", () => {
  it("returns null for online worker", () => {
    const workers = new Map([["1", { id: 1n, online: true }]]);
    expect(workerUnavailableReason(workers, 1n)).toBeNull();
  });

  it("returns reason for offline worker", () => {
    const workers = new Map([["1", { id: 1n, online: false }]]);
    expect(workerUnavailableReason(workers, 1n)).toBeTruthy();
  });

  it("returns reason for unregistered worker", () => {
    const workers = new Map<string, { id: bigint; online: boolean }>();
    expect(workerUnavailableReason(workers, 99n)).toBeTruthy();
  });
});

describe("agent display", () => {
  it("labels claude agent", () => {
    expect(agentLabel(AgentKind.CLAUDE_CODE)).toBe("Claude");
  });

  it("labels codex agent", () => {
    expect(agentLabel(AgentKind.CODEX)).toBe("Codex");
  });
});

describe("agent resolution for inherit label", () => {
  it("resolves from project default", () => {
    const project = create(ProjectSchema, {
      id: 1n, bucketId: 1n, name: "p",
      defaultAgent: AgentKind.CODEX,
    });
    const resolved = resolveAgent(project, undefined);
    expect(resolved.agent).toBe(AgentKind.CODEX);
    expect(resolved.source).toBe("project");
  });

  it("resolves from bucket when project has no default", () => {
    const project = create(ProjectSchema, { id: 1n, bucketId: 1n, name: "p" });
    const bucket = create(BucketSchema, { id: 1n, name: "b", defaultAgent: AgentKind.CODEX });
    const resolved = resolveAgent(project, bucket);
    expect(resolved.agent).toBe(AgentKind.CODEX);
    expect(resolved.source).toBe("bucket");
  });

  it("falls back to claude when nothing configured", () => {
    const project = create(ProjectSchema, { id: 1n, bucketId: 1n, name: "p" });
    const bucket = create(BucketSchema, { id: 1n, name: "b" });
    const resolved = resolveAgent(project, bucket);
    expect(resolved.agent).toBe(AgentKind.CLAUDE_CODE);
    expect(resolved.source).toBe("fallback");
  });
});

describe("model profile resolution for inherit label", () => {
  it("resolves from project", () => {
    const project = create(ProjectSchema, { id: 1n, bucketId: 1n, name: "p", modelProfileId: 5n });
    const resolved = resolveModelProfile(project, undefined);
    expect(resolved?.profileId).toBe(5n);
    expect(resolved?.source).toBe("project");
  });

  it("resolves from bucket when project has none", () => {
    const project = create(ProjectSchema, { id: 1n, bucketId: 1n, name: "p" });
    const bucket = create(BucketSchema, { id: 1n, name: "b", modelProfileId: 7n });
    const resolved = resolveModelProfile(project, bucket);
    expect(resolved?.profileId).toBe(7n);
    expect(resolved?.source).toBe("bucket");
  });

  it("returns undefined when nothing configured", () => {
    const project = create(ProjectSchema, { id: 1n, bucketId: 1n, name: "p" });
    const bucket = create(BucketSchema, { id: 1n, name: "b" });
    expect(resolveModelProfile(project, bucket)).toBeUndefined();
  });
});

describe("permission mode mapping", () => {
  it("inherit maps to UNSPECIFIED", () => {
    expect(PermissionMode.UNSPECIFIED).toBe(0);
  });

  it("DEFAULT and AUTO are distinct", () => {
    expect(PermissionMode.DEFAULT).not.toBe(PermissionMode.AUTO);
    expect(PermissionMode.DEFAULT).toBe(1);
    expect(PermissionMode.AUTO).toBe(2);
  });
});
