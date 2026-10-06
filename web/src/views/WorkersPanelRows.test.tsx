import { create } from "@bufbuild/protobuf";
import { renderToStaticMarkup } from "react-dom/server";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
import { BucketSchema, ConnectMode, WorkerSchema } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { initialState, type AppState } from "@puppet-master/client-core/state/reducer";
import type { PmClient } from "@puppet-master/client-core/ws/client";
import { ClientContext } from "../state/hooks";
import { AddWorkerForm, WorkersPanel } from "./WorkersPanel";

// The page reads the browser origin while rendering the enrollment command.
beforeAll(() => {
  vi.stubGlobal("window", { location: { origin: "https://pm.example.com" } });
});
afterAll(() => {
  vi.unstubAllGlobals();
});

const localHost = create(WorkerSchema, { id: 0n, name: "lima", online: true });

const dialingHost = create(WorkerSchema, {
  id: 4n,
  name: "build-box",
  hostname: "build-box.local",
  online: true,
  connectMode: ConnectMode.DIAL,
});

const dialedHost = create(WorkerSchema, {
  id: 7n,
  name: "garage-box",
  hostname: "garage-box.local",
  online: false,
  lastSeenAtUnixMs: 1n,
  connectMode: ConnectMode.ACCEPT,
  endpoint: "10.0.0.5:7677",
});

const pendingHost = create(WorkerSchema, {
  id: 8n,
  name: "new-box",
  online: false,
  connectMode: ConnectMode.ACCEPT,
  endpoint: "10.0.0.8:7677",
});

function renderHosts(workers: readonly ReturnType<typeof create<typeof WorkerSchema>>[]): string {
  const state: AppState = {
    ...initialState,
    workers: new Map(workers.map((w) => [w.id.toString(), w])),
  };
  const client = {
    subscribe: () => () => {},
    getState: () => state,
  } as unknown as PmClient;
  return renderToStaticMarkup(
    <ClientContext.Provider value={client}>
      <WorkersPanel />
    </ClientContext.Provider>,
  );
}

describe("Workers rows", () => {
  it("labels which end opens the connection, with the address only for a dialed Worker", () => {
    const html = renderHosts([dialingHost, dialedHost]);
    expect(html).toContain("dials controller");
    expect(html).toContain("controller dials");
    expect(html).toContain("10.0.0.5:7677");
  });

  it("gives the local Worker no connection mode, since it has no connection", () => {
    expect(renderHosts([localHost])).not.toContain("worker-connect-mode");
    expect(renderHosts([dialingHost])).toContain("worker-connect-mode");
  });

  it("explains an offline dialed Worker as one the controller cannot reach", () => {
    expect(renderHosts([dialedHost])).toContain(
      "the controller cannot reach this Worker at 10.0.0.5:7677",
    );
  });

  it("offers re-enroll on a remote Worker but never on the local one", () => {
    expect(renderHosts([dialingHost])).toContain("Re-enroll");
    expect(renderHosts([localHost])).not.toContain("Re-enroll");
  });

  it("asks before removing a Worker", () => {
    expect(renderHosts([dialingHost])).toMatch(/aria-haspopup="dialog"[^>]*>Remove</);
  });

  it("shows a newly added Worker as pending, with why, and already allows re-enrollment", () => {
    const html = renderHosts([pendingHost]);
    expect(html).toContain('data-status="pending"');
    expect(html).toContain("enroll command not run yet");
    expect(html).toContain("10.0.0.8:7677");
    expect(html).toContain("Re-enroll");
  });

  it("says re-enrolling keeps the Worker, which is what tells it apart from re-adding", () => {
    const html = renderHosts([dialingHost]);
    expect(html).toContain("Rotate this Worker&#x27;s credential and pinned key");
  });

  it("lists status, platform, connection and version as columns", () => {
    const html = renderHosts([dialingHost]);
    for (const column of ["Worker", "Status", "Platform", "Connection", "Version"]) {
      expect(html).toContain(`<th>${column}</th>`);
    }
  });

  it("keeps both dialogs closed until one is asked for, and says so when there is nothing to list", () => {
    const html = renderHosts([]);
    expect(html).toContain("No workers registered");
    expect(html).not.toContain('role="dialog"');
    expect(html).not.toContain("add-worker-title");
    expect(renderHosts([dialingHost])).not.toContain('role="dialog"');
  });

  it("puts a filter above the table, and no pager while one page holds every worker", () => {
    const html = renderHosts([localHost, dialingHost]);
    expect(html).toContain('aria-label="Filter workers"');
    expect(html).not.toContain("ui-pager");
    expect(renderHosts([])).not.toContain('aria-label="Filter workers"');
  });

  it("pages a long list 25 at a time with the built-in worker first", () => {
    const fleet = Array.from({ length: 30 }, (_, index) =>
      create(WorkerSchema, { id: BigInt(index + 1), name: `box-${index + 1}`, online: true }),
    );
    const html = renderHosts([...fleet, localHost]);
    expect(html).toContain("1–25 of 31 workers");
    expect(html).toContain("Page 1 of 2");
    expect(html.match(/class="worker-row"/g)).toHaveLength(25);
    expect(html.indexOf('data-worker-id="0"')).toBeLessThan(html.indexOf('data-worker-id="1"'));
    expect(html).not.toContain('data-worker-id="25"');
  });
});

describe("add worker form", () => {
  // The location fields read the browser origin, which is stubbed per run.
  let html = "";
  beforeAll(() => {
    html = renderToStaticMarkup(
      <AddWorkerForm
        build={null}
        workers={[]}
        copied={null}
        onCopy={() => {}}
        onClose={() => {}}
      />,
    );
  });

  it("defaults to a local worker, which dials the controller, so no address is asked for", () => {
    expect(html).toMatch(/data-location="local"[^>]*aria-pressed="true"/);
    expect(html).not.toContain('value="controller-dials"');
    expect(html).toContain("Where it runs");
    expect(html).not.toContain("Worker address");
  });

  it("defaults to a local worker, which is asked for no controller URL", () => {
    expect(html).toContain('data-location="local" title="Local (same machine as the controller)" aria-pressed="true"');
    expect(html).toContain('data-location="remote" title="Remote (another machine)" aria-pressed="false"');
    expect(html).not.toContain("Controller URL");
  });

  it("asks what holds the worker, with no entry for another machine", () => {
    for (const holder of ["Directly on the machine", "Docker container", "Podman container", "Incus container", "Lima VM"]) {
      expect(html).toContain(`>${holder}</option>`);
    }
    expect(html).not.toContain("Another machine");
  });

  it("asks where the worker runs first, and not which side connects while it is local", () => {
    expect(html).toContain('aria-label="Location"');
    expect(html).not.toContain("Which side opens the connection?");
    expect(html).not.toContain("Controller → Worker");
  });

  it("shows no command before one is generated", () => {
    expect(html).not.toContain("pm worker");
    expect(html).toContain("Generate command");
  });
});

describe("worker bucket access", () => {
  it("offers only buckets with no access selected by default", () => {
    const html = renderToStaticMarkup(
      <AddWorkerForm
        build={null}
        workers={[]}
        buckets={[create(BucketSchema, { id: 1n, name: "Work" }), create(BucketSchema, { id: 2n, name: "Personal" })]}
        copied={null}
        onCopy={() => {}}
        onClose={() => {}}
      />,
    );
    expect(html).toContain("Bucket Access");
    expect(html).toContain("Select buckets (optional)");
    expect(html).toContain("Selecting a bucket lets all projects within it use this worker");
    expect(html).toContain("specific buckets or projects afterward");
    expect(html).toContain(">Work</span>");
    expect(html).toContain(">Personal</span>");
    expect(html.match(/type="checkbox"/g)).toHaveLength(2);
    expect(html).not.toContain("checked=");
  });

  it("explains how to configure access when no buckets exist", () => {
    const html = renderToStaticMarkup(
      <AddWorkerForm build={null} workers={[]} copied={null} onCopy={() => {}} onClose={() => {}} />,
    );
    expect(html).toContain("No buckets yet. You can configure access after creating one.");
    expect(html).not.toContain('type="checkbox"');
  });
});
