import { describe, expect, it } from "vitest";
import {
  hostConnectionView,
  hostRuntimeView,
  isDialedHost,
  offlineHostReason,
  orderWorkers,
  resolveProjectWorkerId,
  workerUnavailableReason,
} from "./worker";
import { ConnectMode } from "../gen/pm/v1/pm_pb";

const LOCAL = 0n;
const DIALS = { connectMode: ConnectMode.DIAL, endpoint: "" };
const DIALED = { connectMode: ConnectMode.ACCEPT, endpoint: "10.0.0.5:7677" };

describe("resolveProjectWorkerId", () => {
  it("uses the project's own override when set", () => {
    expect(resolveProjectWorkerId({ workerId: 5n }, { defaultWorkerId: 3n })).toBe(5n);
  });

  it("honors an explicit local override over the bucket default", () => {
    expect(resolveProjectWorkerId({ workerId: LOCAL }, { defaultWorkerId: 3n })).toBe(LOCAL);
  });

  it("falls back to the bucket default when the project inherits", () => {
    expect(resolveProjectWorkerId({ workerId: undefined }, { defaultWorkerId: 3n })).toBe(3n);
  });

  it("falls back to the local worker with no project or bucket", () => {
    expect(resolveProjectWorkerId(undefined, undefined)).toBe(LOCAL);
  });
});

describe("orderWorkers", () => {
  it("places the local worker first and sorts the rest by id", () => {
    const ordered = orderWorkers([{ id: 5n }, { id: 0n }, { id: 2n }]);
    expect(ordered.map((w) => w.id)).toEqual([0n, 2n, 5n]);
  });

  it("does not mutate the input", () => {
    const input = [{ id: 3n }, { id: 1n }];
    orderWorkers(input);
    expect(input.map((w) => w.id)).toEqual([3n, 1n]);
  });
});

describe("isDialedHost", () => {
  it("is true only when the controller opens the connection", () => {
    expect(isDialedHost(DIALED)).toBe(true);
    expect(isDialedHost(DIALS)).toBe(false);
    expect(isDialedHost(undefined)).toBe(false);
    expect(isDialedHost({})).toBe(false);
  });
});

describe("hostConnectionView", () => {
  it("names the local host as neither end, since it has no connection", () => {
    expect(hostConnectionView({ id: LOCAL, ...DIALS })).toBeNull();
  });

  it("shows a dialing host without an address", () => {
    expect(hostConnectionView({ id: 2n, ...DIALS })).toEqual({
      label: "dials controller",
      endpoint: "",
      title: "this Worker opens the connection to the controller",
    });
  });

  it("shows a dialed host with the address the controller reaches it at", () => {
    expect(hostConnectionView({ id: 2n, ...DIALED })).toEqual({
      label: "controller dials",
      endpoint: "10.0.0.5:7677",
      title: "the controller opens the connection to 10.0.0.5:7677",
    });
  });

  it("still reads as dialed when no address was recorded", () => {
    const view = hostConnectionView({ id: 2n, connectMode: ConnectMode.ACCEPT, endpoint: "" });
    expect(view?.label).toBe("controller dials");
    expect(view?.title).toBe("the controller opens the connection, but no address is recorded");
  });
});

describe("offlineHostReason", () => {
  it("blames the controller's reach for a dialed host", () => {
    expect(offlineHostReason(DIALED)).toBe(
      "the controller cannot reach this Worker at 10.0.0.5:7677",
    );
    expect(offlineHostReason({ connectMode: ConnectMode.ACCEPT, endpoint: "" })).toBe(
      "the controller cannot reach this Worker",
    );
  });

  it("says a dialing host has not connected", () => {
    expect(offlineHostReason(DIALS)).toBe("this Worker has not connected to the controller");
  });
});

describe("workerUnavailableReason", () => {
  it("explains an empty remote-only controller", () => {
    expect(workerUnavailableReason(new Map(), LOCAL)).toBe(
      "no workers registered; local worker is disabled for this daemon",
    );
  });

  it("distinguishes disabled local, unregistered, and online workers", () => {
    const workers = new Map([
      ["2", { online: false, ...DIALS }],
      ["3", { online: true, ...DIALS }],
    ]);
    expect(workerUnavailableReason(workers, LOCAL)).toBe("local worker is disabled for this daemon");
    expect(workerUnavailableReason(workers, 9n)).toBe("worker 9 is not registered");
    expect(workerUnavailableReason(workers, 3n)).toBeNull();
  });

  it("distinguishes an unreached dialed host from one that never dialed in", () => {
    const workers = new Map([
      ["2", { online: false, ...DIALS }],
      ["4", { online: false, ...DIALED }],
      ["5", { online: false, connectMode: ConnectMode.ACCEPT, endpoint: "" }],
    ]);
    expect(workerUnavailableReason(workers, 2n)).toBe("worker 2 has not connected");
    expect(workerUnavailableReason(workers, 4n)).toBe(
      "worker 4 cannot be reached at 10.0.0.5:7677",
    );
    expect(workerUnavailableReason(workers, 5n)).toBe("worker 5 cannot be reached");
  });

  it("keeps the local worker's own wording whatever mode it carries", () => {
    const workers = new Map([["0", { online: false, ...DIALED }]]);
    expect(workerUnavailableReason(workers, LOCAL)).toBe("local worker is offline");
  });
});

describe("hostRuntimeView", () => {
  it("shows nothing for a Host that reports no runtime", () => {
    // Every Host looked like this before they reported one, so an empty
    // report must not read as a claim about the machine.
    expect(hostRuntimeView({})).toBeNull();
    expect(hostRuntimeView({ runtime: "", container: "" })).toBeNull();
  });

  it("names the runtime and the container, and how to read its log", () => {
    const view = hostRuntimeView({ runtime: "docker", container: "pm-worker-repos" });
    expect(view?.label).toBe("docker:pm-worker-repos");
    expect(view?.title).toContain("docker logs -f pm-worker-repos");
  });

  it("falls back to the runtime alone when no container name came with it", () => {
    expect(hostRuntimeView({ runtime: "incus" })).toEqual({
      label: "incus",
      title: "this Host runs under incus",
    });
  });
});
