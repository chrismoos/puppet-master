import { describe, expect, it } from "vitest";
import { create } from "@bufbuild/protobuf";
import { controllerBase } from "../api/workers";
import {
  controllerBuildVersion,
  filterWorkers,
  projectCreationWorkerSelection,
  hostCommand,
  reenrollDefaults,
  workerStatusView,
  workerVersionView,
} from "./WorkersPanel";
import { devicePlatformLabel, mobileDeviceLastSeen } from "./MobileDevicesPanel";
import { ConnectMode, WorkerSchema } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { catalogModeForRoute, nextProjectDefaultHost, workerPathUpdates } from "./ProjectsCatalog";

describe("Workers row pm version", () => {
  const controller = controllerBuildVersion({ version: "0.1.0", gitRev: "abc1234" });

  it("joins the controller build from version and git revision", () => {
    expect(controller).toBe("0.1.0+abc1234");
    expect(controllerBuildVersion(null)).toBeNull();
  });

  it("shows a matching build without drift", () => {
    expect(workerVersionView("0.1.0+abc1234", controller)).toEqual({
      label: "pm 0.1.0+abc1234",
      drift: false,
      actionable: false,
    });
  });

  it("flags another build of the same release without offering an update", () => {
    expect(workerVersionView("0.1.0+0ldrev1", controller)).toEqual({
      label: "pm 0.1.0+0ldrev1",
      drift: true,
      actionable: false,
      title:
        "built from a different commit than controller pm 0.1.0+abc1234; the release channel publishes one build per version, so there is nothing to install",
    });
  });

  it("offers an update only when the releases themselves differ", () => {
    expect(workerVersionView("0.0.9+0ldrev1", controller)).toEqual({
      label: "pm 0.0.9+0ldrev1",
      drift: true,
      actionable: true,
      title: "differs from controller pm 0.1.0+abc1234",
    });
  });

  it("reads a worker that never reported a build as unknown, never as drift", () => {
    expect(workerVersionView("", controller)).toEqual({
      label: "pm version unknown",
      drift: false,
      actionable: false,
    });
  });

  it("never flags drift while the controller build is unknown", () => {
    expect(workerVersionView("0.1.0+abc1234", null)).toEqual({
      label: "pm 0.1.0+abc1234",
      drift: false,
      actionable: false,
    });
  });
});

describe("re-enroll command", () => {
  it("keeps a dialing host dialing the controller", () => {
    expect(
      hostCommand(
        reenrollDefaults({ connectMode: ConnectMode.DIAL, endpoint: "", name: "build", platform: "linux", runtime: "" }),
        "https://pm.example.com",
        "rot8",
        7677,
      ),
    ).toBe("pm worker --name build --controller wss://pm.example.com:7677 --token rot8");
  });

  it("keeps a dialed host waiting on the address the controller dials", () => {
    expect(
      hostCommand(
        reenrollDefaults({
          connectMode: ConnectMode.ACCEPT,
          endpoint: "10.0.0.5:7677",
          name: "edge",
          platform: "linux",
          runtime: "",
        }),
        "https://pm.example.com",
        "rot8",
        7677,
      ),
    ).toBe("pm worker --name edge --listen 10.0.0.5:7677 --token rot8");
  });
});

describe("project creation worker selection", () => {
  it("stores no override and browses the bucket worker when inheriting", () => {
    expect(projectCreationWorkerSelection("", { defaultWorkerId: 3n })).toEqual({
      override: undefined,
      effective: 3n,
    });
  });

  it("uses an explicit worker for both creation and directory browsing", () => {
    expect(projectCreationWorkerSelection("5", { defaultWorkerId: 3n })).toEqual({
      override: 5n,
      effective: 5n,
    });
  });
});

describe("Projects catalog routing", () => {
  it("defaults to projects and honors an explicit catalog", () => {
    expect(catalogModeForRoute(undefined)).toBe("projects");
    expect(catalogModeForRoute("buckets")).toBe("buckets");
  });

  it("lets direct entity routes choose the matching catalog", () => {
    expect(catalogModeForRoute("projects", "3")).toBe("buckets");
    expect(catalogModeForRoute("buckets", "3", "9")).toBe("projects");
  });
});

describe("mobile device last seen", () => {
  const NOW = 1_700_000_000_000;
  const MINUTE_MS = 60_000;
  const DAY_MS = 86_400_000;

  it("reads a device that never connected as never, and not as recent", () => {
    expect(mobileDeviceLastSeen({ lastSeenAtUnixMs: null }, NOW)).toEqual({ label: "never", recent: false });
  });

  it("reads activity inside the last day as an age and marks it recent", () => {
    expect(mobileDeviceLastSeen({ lastSeenAtUnixMs: NOW - 4 * MINUTE_MS }, NOW))
      .toEqual({ label: "4m ago", recent: true });
  });

  it("reads older activity as a date", () => {
    const seen = mobileDeviceLastSeen({ lastSeenAtUnixMs: NOW - 3 * DAY_MS }, NOW);
    expect(seen.recent).toBe(false);
    expect(seen.label).not.toContain("ago");
    expect(seen.label).toMatch(/2023/);
  });
});

describe("device platform label", () => {
  it("names the platforms a client reports", () => {
    expect(devicePlatformLabel("ios")).toBe("iOS");
    expect(devicePlatformLabel("cli")).toBe("Command line");
    expect(devicePlatformLabel("Android")).toBe("Android");
  });

  it("shows a platform it does not know as it was sent", () => {
    expect(devicePlatformLabel("visionOS")).toBe("visionOS");
  });
});

describe("Workers row status", () => {
  const remote = { id: 4n, online: false, lastSeenAtUnixMs: 1n, connectMode: ConnectMode.DIAL, endpoint: "" };

  it("reads an online worker as online with no reason", () => {
    expect(workerStatusView({ ...remote, online: true })).toEqual({ label: "Online", tone: "ok" });
  });

  it("says a worker that never connected is waiting for its enroll command", () => {
    expect(workerStatusView({ ...remote, lastSeenAtUnixMs: undefined })).toEqual({
      label: "Pending",
      tone: "warn",
      reason: "enroll command not run yet",
    });
  });

  it("explains an offline worker by which end opens the connection", () => {
    expect(workerStatusView(remote).reason).toBe("this Worker has not connected to the controller");
    expect(workerStatusView({ ...remote, connectMode: ConnectMode.ACCEPT, endpoint: "10.0.0.5:7677" }).reason)
      .toBe("the controller cannot reach this Worker at 10.0.0.5:7677");
  });

  it("never reads the local worker as pending", () => {
    expect(workerStatusView({ ...remote, id: 0n, lastSeenAtUnixMs: undefined }))
      .toEqual({ label: "Offline", tone: "bad" });
  });
});

describe("project host path reconciliation", () => {
  const stored = [{ workerId: 5n, path: "/laptop/repo" }];

  it("sets a new explicit path and rewrites a changed one", () => {
    expect(workerPathUpdates(stored, { "5": "/laptop/repo", "7": " /vm/repo " }, ["5", "7"])).toEqual([
      { workerId: 7n, path: "/vm/repo" },
    ]);
    expect(workerPathUpdates(stored, { "5": "/laptop/other" }, ["5"])).toEqual([
      { workerId: 5n, path: "/laptop/other" },
    ]);
  });

  it("clears an emptied stored row and leaves never-set hosts alone", () => {
    expect(workerPathUpdates(stored, { "5": "  " }, ["5", "7"])).toEqual([{ workerId: 5n, path: null }]);
    expect(workerPathUpdates(stored, {}, ["5", "7"])).toEqual([{ workerId: 5n, path: null }]);
    expect(workerPathUpdates([], {}, ["7"])).toEqual([]);
  });

  it("never writes a host outside the allowed set", () => {
    expect(workerPathUpdates(stored, { "5": "/laptop/repo", "9": "/gone" }, ["5"])).toEqual([]);
  });
});

describe("project default host after an allowed-host edit", () => {
  it("keeps a still-allowed selection", () => {
    expect(nextProjectDefaultHost(["5", "7"], "7", "5")).toBe("7");
    expect(nextProjectDefaultHost(["5", "7"], "", "5")).toBe("");
  });

  it("moves an override off a host that was just unchecked", () => {
    expect(nextProjectDefaultHost(["5"], "7", "5")).toBe("5");
  });

  it("pins an inheriting project when the bucket default is unchecked", () => {
    expect(nextProjectDefaultHost(["7"], "", "5")).toBe("7");
  });
});

describe("enrollment command", () => {
  const origin = "https://pm.example.com";

  /** The name is how every later `pm worker` command addresses it. */
  it("carries the label as the worker's name", () => {
    expect(
      hostCommand(
        { direction: "worker-dials", workerType: "machine", location: "local", controllerUrl: "", endpoint: "", name: "build", platform: "linux" },
        origin,
        "tok",
        7677,
      ),
    ).toBe("pm worker --name build --controller wss://pm.example.com:7677 --token tok");
  });

  it("launches a container for a local worker type that is one, reached through its guest name", () => {
    expect(
      hostCommand(
        { direction: "worker-dials", workerType: "docker", location: "local", controllerUrl: "", endpoint: "", name: "box", platform: "linux" },
        origin,
        "tok",
        7677,
      ),
    ).toBe(
      "pm worker --sandbox --runtime docker --name box " +
        "--controller wss://host.docker.internal:7677 --token tok",
    );
  });

  it("uses each runtime's own name for the host, where it has one", () => {
    const command = (workerType: "podman" | "incus") =>
      hostCommand(
        {
          direction: "worker-dials",
          workerType,
          location: "local",
          controllerUrl: "",
          endpoint: "",
          name: "n",
          platform: "linux",
        },
        origin,
        "tok",
        7677,
      );
    expect(command("podman")).toContain("--runtime podman");
    expect(command("podman")).toContain("wss://host.containers.internal:7677");
    // Incus has no well-known name for the host, so the controller's own
    // address stands and the launcher resolves it.
    expect(command("incus")).toContain("--runtime incus");
    expect(command("incus")).toContain("wss://pm.example.com:7677");
  });

  it("quotes a label that would not survive a shell", () => {
    expect(
      hostCommand(
        {
          direction: "worker-dials",
          workerType: "machine",
          location: "local",
          controllerUrl: "",
          endpoint: "",
          name: "my box",
        platform: "linux",
        },
        origin,
        "tok",
        7677,
      ),
    ).toContain("--name 'my box'");
  });

  it("omits the name when there is none, rather than passing an empty one", () => {
    expect(
      hostCommand(
        { direction: "worker-dials", workerType: "machine", location: "local", controllerUrl: "", endpoint: "", name: "  ", platform: "linux" },
        origin,
        "tok",
        7677,
      ),
    ).toBe("pm worker --controller wss://pm.example.com:7677 --token tok");
  });
});

describe("a remote worker's command", () => {
  const origin = "https://pm.example.com";
  const remote = {
    direction: "worker-dials",
    location: "remote",
    controllerUrl: null,
    endpoint: "",
    name: "box",
    platform: "linux",
  } as const;

  /// A guest's name for its host resolves only on the controller's own
  /// machine, so a container somewhere else has to dial the real address.
  it("dials the controller's own address from a container, never a guest name", () => {
    for (const workerType of ["docker", "podman", "incus"] as const) {
      expect(hostCommand({ ...remote, workerType }, origin, "tok", 7677)).toBe(
        `pm worker --sandbox --runtime ${workerType} --name box ` +
          "--controller wss://pm.example.com:7677 --token tok",
      );
    }
  });

  it("uses the address the operator entered, corrected onto wss", () => {
    expect(
      hostCommand(
        { ...remote, workerType: "docker", controllerUrl: "http://100.64.0.7:7677/" },
        origin,
        "tok",
        7677,
      ),
    ).toBe(
      "pm worker --sandbox --runtime docker --name box " +
        "--controller wss://100.64.0.7:7677 --token tok",
    );
  });

  it("falls back to the controller's own address when the entry is not a full URL", () => {
    for (const controllerUrl of ["", "100.64.0.7:7677"]) {
      expect(
        hostCommand({ ...remote, workerType: "machine", controllerUrl }, origin, "tok", 7677),
      ).toBe("pm worker --name box --controller wss://pm.example.com:7677 --token tok");
    }
  });

  /// A Lima VM is a guest of the controller's machine, so a remote choice
  /// that still carries it runs on the machine itself at the real address.
  it("never hands a remote worker the Lima host name", () => {
    expect(hostCommand({ ...remote, workerType: "lima" }, origin, "tok", 7677)).toBe(
      "pm worker --name box --controller wss://pm.example.com:7677 --token tok",
    );
    expect(
      hostCommand({ ...remote, location: "local", workerType: "lima" }, origin, "tok", 7677),
    ).toBe("pm worker --name box --controller wss://host.lima.internal:7677 --token tok");
  });

  it("leaves the address out of a command the controller dials", () => {
    expect(
      hostCommand(
        { ...remote, direction: "controller-dials", workerType: "docker", endpoint: "10.0.0.5:7677" },
        origin,
        "tok",
        7677,
      ),
    ).toBe("pm worker --sandbox --runtime docker --name box --listen 10.0.0.5:7677 --token tok");
  });
});

describe("re-enrolling a containerized host", () => {
  const reported = {
    connectMode: ConnectMode.DIAL,
    endpoint: "",
    name: "repos",
    platform: "linux",
    runtime: "docker",
  };

  /// Re-enrolling defaulted to "this machine" regardless of what the
  /// host was, so a container worker was handed a command that would
  /// have run pm beside its container instead of inside it.
  it("starts from the runtime and platform the host reported", () => {
    const choice = reenrollDefaults(reported);
    expect(choice.workerType).toBe("docker");
    expect(choice.platform).toBe("linux");
    expect(hostCommand(choice, "https://pm.example.com", "rot8", 7677)).toContain(
      "pm worker --sandbox --runtime docker --name repos ",
    );
  });

  /// Nothing a worker reports places it on the controller's machine, and
  /// a guest name handed to a container elsewhere would not resolve.
  it("starts as remote, dialing the controller's own address", () => {
    const choice = reenrollDefaults(reported);
    expect(choice.location).toBe("remote");
    expect(hostCommand(choice, "https://pm.example.com", "rot8", 7677)).toBe(
      "pm worker --sandbox --runtime docker --name repos " +
        "--controller wss://pm.example.com:7677 --token rot8",
    );
  });

  it("uses the guest name once the operator says the container is local", () => {
    const choice = { ...reenrollDefaults(reported), location: "local" as const };
    expect(hostCommand(choice, "https://pm.example.com", "rot8", 7677)).toBe(
      "pm worker --sandbox --runtime docker --name repos " +
        "--controller wss://host.docker.internal:7677 --token rot8",
    );
  });

  it("treats a host that reported no runtime as running on its machine", () => {
    const choice = reenrollDefaults({ ...reported, name: "build", platform: "macos", runtime: "" });
    expect(choice.workerType).toBe("machine");
    expect(choice.platform).toBe("macos");
    expect(hostCommand(choice, "https://pm.example.com", "rot8", 7677)).not.toContain("--sandbox");
  });

  /// Incus cannot run on macOS, so a host whose reported pair is
  /// impossible must not produce a command that machine cannot run.
  it("drops a runtime its platform cannot run", () => {
    const choice = reenrollDefaults({ ...reported, name: "odd", platform: "macos", runtime: "incus" });
    expect(hostCommand(choice, "https://pm.example.com", "rot8", 7677)).not.toContain("incus");
  });
});

describe("Workers filter", () => {
  const local = create(WorkerSchema, {
    id: 0n,
    name: "controller",
    hostname: "pm-host",
    platform: "macos",
    online: true,
    pmVersion: "0.9.11+abc1234",
  });
  const container = create(WorkerSchema, {
    id: 4n,
    name: "build-box",
    hostname: "4f2a9c1d7b3e",
    platform: "linux",
    runtime: "docker",
    container: "pm-build-box",
    online: true,
    connectMode: ConnectMode.DIAL,
    pmVersion: "0.9.10+0ldrev1",
  });
  const dialed = create(WorkerSchema, {
    id: 7n,
    name: "garage",
    hostname: "garage.local",
    platform: "linux",
    online: false,
    lastSeenAtUnixMs: 1n,
    connectMode: ConnectMode.ACCEPT,
    endpoint: "10.0.0.5:7677",
  });
  const pending = create(WorkerSchema, { id: 8n, name: "new-box", connectMode: ConnectMode.DIAL });
  const all = [local, container, dialed, pending];
  const names = (query: string) =>
    filterWorkers(all, query, "0.9.11+abc1234").map((worker) => worker.name);

  it("keeps every worker, in order, for an empty filter", () => {
    expect(names("")).toEqual(["controller", "build-box", "garage", "new-box"]);
    expect(names("   ")).toHaveLength(all.length);
  });

  it("matches name and hostname without regard to case", () => {
    expect(names("BUILD")).toEqual(["build-box"]);
    expect(names("garage.local")).toEqual(["garage"]);
  });

  it("matches platform, runtime, and container", () => {
    expect(names("macos")).toEqual(["controller"]);
    expect(names("docker")).toEqual(["build-box"]);
    expect(names("pm-build-box")).toEqual(["build-box"]);
  });

  it("matches the connection mode as the row words it, and a dialed address", () => {
    expect(names("dials controller")).toEqual(["build-box", "new-box"]);
    expect(names("controller dials")).toEqual(["garage"]);
    expect(names("10.0.0.5")).toEqual(["garage"]);
    expect(names("built in")).toEqual(["controller"]);
  });

  it("matches status and version", () => {
    expect(names("offline")).toEqual(["garage"]);
    expect(names("pending")).toEqual(["new-box"]);
    expect(names("0.9.10")).toEqual(["build-box"]);
    expect(names("unknown")).toEqual(["garage", "new-box"]);
  });

  /// Words spread over several columns would make "dials controller" match
  /// a worker the controller dials, which is the opposite of what was asked.
  it("looks for the phrase inside one column, not its words across columns", () => {
    expect(names("  Dials   Controller ")).toEqual(["build-box", "new-box"]);
    expect(names("linux online")).toEqual([]);
  });
});

describe("the address a Worker command dials", () => {
  const browser = "https://127.0.0.1:8080";

  it("is the daemon's configured public URL, as an origin", () => {
    expect(controllerBase({ publicUrl: "https://pm.example.com/" }, browser)).toBe("https://pm.example.com");
    expect(controllerBase({ publicUrl: "https://pm.example.com:8443/pm" }, browser)).toBe("https://pm.example.com:8443");
  });

  it("falls back to the browser's origin without one, or with one that is not a URL", () => {
    expect(controllerBase(null, browser)).toBe(browser);
    expect(controllerBase({ publicUrl: null }, browser)).toBe(browser);
    expect(controllerBase({ publicUrl: "  " }, browser)).toBe(browser);
    expect(controllerBase({ publicUrl: "pm.example.com" }, browser)).toBe(browser);
  });
});
