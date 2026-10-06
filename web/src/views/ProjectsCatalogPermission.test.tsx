import { create } from "@bufbuild/protobuf";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  BucketSchema,
  PermissionMode,
  ProjectSchema,
  WorkerSchema,
} from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { initialState, type AppState } from "@puppet-master/client-core/state/reducer";
import type { PmClient } from "@puppet-master/client-core/ws/client";
import { ClientContext } from "../state/hooks";
import { ProjectsCatalog, permissionFromValue, permissionValue } from "./ProjectsCatalog";

const host = create(WorkerSchema, { id: 0n, name: "lima", online: true });

function bucket(permissionMode: PermissionMode) {
  return create(BucketSchema, {
    id: 1n,
    name: "opensource",
    defaultWorkerId: 0n,
    allowedWorkerIds: [0n],
    permissionMode,
  });
}

function project(permissionMode: PermissionMode) {
  return create(ProjectSchema, {
    id: 5n,
    bucketId: 1n,
    name: "puppet-master",
    path: "/src/pm",
    allowedWorkerIds: [0n],
    permissionMode,
  });
}

function render(
  bucketMode: PermissionMode,
  projectMode: PermissionMode,
  selection: { bucket?: string; project?: string },
): string {
  const state: AppState = {
    ...initialState,
    workers: new Map([[host.id.toString(), host]]),
    buckets: new Map([["1", bucket(bucketMode)]]),
    projects: new Map([["5", project(projectMode)]]),
  };
  const client = {
    subscribe: () => () => {},
    getState: () => state,
  } as unknown as PmClient;
  return renderToStaticMarkup(
    <ClientContext.Provider value={client}>
      <ProjectsCatalog
        catalog={selection.project ? "projects" : "buckets"}
        selectedBucketId={selection.bucket}
        selectedProjectId={selection.project}
      />
    </ClientContext.Provider>,
  );
}

describe("permission mode form values", () => {
  it("round-trips every real mode", () => {
    for (const mode of [PermissionMode.DEFAULT, PermissionMode.AUTO, PermissionMode.BYPASS]) {
      expect(permissionFromValue(permissionValue(mode))).toBe(mode);
    }
  });

  it("maps an unset project override to the empty option, and back", () => {
    expect(permissionValue(PermissionMode.UNSPECIFIED)).toBe("");
    expect(permissionFromValue("")).toBe(PermissionMode.UNSPECIFIED);
  });
});

describe("project drawer", () => {
  it("offers the permission mode a project row used to own", () => {
    const html = render(PermissionMode.DEFAULT, PermissionMode.UNSPECIFIED, { project: "5" });
    expect(html).toContain("Permission mode");
    expect(html).toContain(">bypass<");
  });

  it("names the bucket and its resolved mode on the inherit option", () => {
    const html = render(PermissionMode.BYPASS, PermissionMode.UNSPECIFIED, { project: "5" });
    expect(html).toContain("Inherit opensource (bypass)");
  });

  it("reads an unset bucket as the interactive default rather than blank", () => {
    const html = render(PermissionMode.UNSPECIFIED, PermissionMode.UNSPECIFIED, { project: "5" });
    expect(html).toContain("Inherit opensource (default)");
  });
});

describe("bucket drawer", () => {
  it("has no inherit option, since a bucket is the floor of the cascade", () => {
    const html = render(PermissionMode.DEFAULT, PermissionMode.UNSPECIFIED, { bucket: "1" });
    expect(html).toContain("Permission mode");
    expect(html).not.toContain("Inherit opensource");
  });
});
