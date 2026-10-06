import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import {
  controllerOrigin,
  enrollCommand,
  installSteps,
  offeredAt,
  platformOf,
  resolveWorkerType,
  runsOn,
  enrollWorker,
  listenCommand,
  normalizeControllerUrl,
  normalizeEndpoint,
  reenrollWorker,
  removeWorker,
  setBucketWorker,
  setProjectWorker,
} from "./workers";
import { seedAccessToken } from "./token.fixture";

// Every authenticated call mints a token when it holds none, which would
// otherwise be the first call a stub answers.
beforeEach(seedAccessToken);

function jsonResponse(body: unknown, init?: { status?: number }): Response {
  return new Response(JSON.stringify(body), {
    status: init?.status ?? 200,
    headers: { "Content-Type": "application/json" },
  });
}

afterEach(() => {
  vi.unstubAllGlobals();
});

function stubFetch(res: Response) {
  const fetchMock = vi.fn().mockResolvedValue(res);
  vi.stubGlobal("fetch", fetchMock);
  return fetchMock;
}

function lastBody(fetchMock: ReturnType<typeof vi.fn>): unknown {
  const init = fetchMock.mock.calls[0][1] as RequestInit;
  return JSON.parse(init.body as string);
}

describe("enrollCommand", () => {
  it("builds a copy-paste worker command against the host plane", () => {
    expect(enrollCommand("https://pm.example.com", "abc123", "machine", 7677)).toBe(
      "pm worker --controller wss://pm.example.com:7677 --token abc123",
    );
  });

  it("uses the Lima host while preserving protocol and port", () => {
    expect(enrollCommand("http://127.0.0.1:7676", "abc123", "lima", 7677)).toBe(
      "pm worker --controller wss://host.lima.internal:7677 --token abc123",
    );
  });

});

describe("listenCommand", () => {
  it("makes the host wait on its address instead of dialing the controller", () => {
    expect(listenCommand("10.0.0.5:7677", "abc123")).toBe(
      "pm worker --listen 10.0.0.5:7677 --token abc123",
    );
    expect(listenCommand("10.0.0.5:7677", "abc123")).not.toContain("--controller");
  });
});

describe("normalizeEndpoint", () => {
  it("keeps a host:port address and trims it", () => {
    expect(normalizeEndpoint("10.0.0.5:7677")).toBe("10.0.0.5:7677");
    expect(normalizeEndpoint("  build-box.tailnet.ts.net:7677  ")).toBe(
      "build-box.tailnet.ts.net:7677",
    );
    expect(normalizeEndpoint("[fd00::1]:7677")).toBe("[fd00::1]:7677");
  });

  it("rejects an address with no port", () => {
    expect(normalizeEndpoint("10.0.0.5")).toBeNull();
    expect(normalizeEndpoint("")).toBeNull();
  });

  it("rejects a URL, because the controller dials the address directly", () => {
    expect(normalizeEndpoint("https://10.0.0.5:7677")).toBeNull();
    expect(normalizeEndpoint("10.0.0.5:7677/worker")).toBeNull();
  });

  it("rejects a port that is not a number in range", () => {
    expect(normalizeEndpoint("10.0.0.5:port")).toBeNull();
    expect(normalizeEndpoint("10.0.0.5:0")).toBeNull();
    expect(normalizeEndpoint("10.0.0.5:70000")).toBeNull();
  });
});

describe("controllerOrigin", () => {
  /// Hosts connect to the host plane, not the web UI, so the browser's own
  /// scheme and port are never the answer — the command has to name the
  /// listener the host will actually reach.
  it("moves the browser origin onto the host plane", () => {
    expect(controllerOrigin("https://pm.example.com:8443", 7677)).toBe(
      "wss://pm.example.com:7677",
    );
  });

  it("replaces the hostname for a guest runtime", () => {
    expect(controllerOrigin("https://pm.example.com:8443", 7677, "host.lima.internal")).toBe(
      "wss://host.lima.internal:7677",
    );
    expect(controllerOrigin("http://127.0.0.1:7676", 7677, "host.docker.internal")).toBe(
      "wss://host.docker.internal:7677",
    );
  });

  it("omits the port when the controller reports none", () => {
    expect(controllerOrigin("https://pm.example.com:8443", null)).toBe(
      "wss://pm.example.com",
    );
  });
});

describe("normalizeControllerUrl", () => {
  /// The host plane speaks only wss, and `pm worker` refuses anything else
  /// rather than upgrading it, so a typed-in scheme is corrected here or the
  /// generated command would fail on paste.
  it("puts any accepted scheme onto wss", () => {
    expect(normalizeControllerUrl("http://192.168.1.20:7677")).toBe(
      "wss://192.168.1.20:7677",
    );
    expect(normalizeControllerUrl("https://pm.tailnet.ts.net")).toBe(
      "wss://pm.tailnet.ts.net",
    );
    expect(normalizeControllerUrl("wss://pm.tailnet.ts.net:7677")).toBe(
      "wss://pm.tailnet.ts.net:7677",
    );
  });

  it("trims whitespace and trailing slashes", () => {
    expect(normalizeControllerUrl("  http://10.0.0.5:7677/  ")).toBe(
      "wss://10.0.0.5:7677",
    );
  });

  it("rejects input without a scheme", () => {
    expect(normalizeControllerUrl("192.168.1.20:7676")).toBeNull();
    expect(normalizeControllerUrl("pm.example.com")).toBeNull();
  });

  it("rejects schemes pm worker cannot dial", () => {
    expect(normalizeControllerUrl("ftp://pm.example.com")).toBeNull();
  });

  it("rejects an empty value", () => {
    expect(normalizeControllerUrl("")).toBeNull();
  });
});

describe("enrollWorker", () => {
  it("sends the selected buckets with enrollment", async () => {
    const fetchMock = stubFetch(jsonResponse({ token: "t0k", expiresAtUnixMs: 42 }));
    await enrollWorker("build-box", undefined, [3n, 8n]);
    expect(lastBody(fetchMock)).toEqual({ label: "build-box", bucket_ids: [3, 8] });
  });

  it("posts the label and returns the token", async () => {
    const fetchMock = stubFetch(jsonResponse({ token: "t0k", expiresAtUnixMs: 42 }));
    const result = await enrollWorker("build-box");
    expect(fetchMock.mock.calls[0][0]).toBe("/api/workers/enroll");
    expect(lastBody(fetchMock)).toEqual({ label: "build-box" });
    expect(result).toEqual({ token: "t0k", expiresAtUnixMs: 42 });
  });

  it("records the address for a host the controller dials", async () => {
    const fetchMock = stubFetch(jsonResponse({ token: "t0k", expiresAtUnixMs: 42 }));
    await enrollWorker("garage-box", { endpoint: "10.0.0.5:7677" });
    expect(lastBody(fetchMock)).toEqual({
      label: "garage-box",
      connect_mode: "accept",
      endpoint: "10.0.0.5:7677",
    });
  });

  it("surfaces the server error message", async () => {
    stubFetch(jsonResponse({ error: "no capacity" }, { status: 400 }));
    await expect(enrollWorker("x")).rejects.toThrow("no capacity");
  });
});

describe("reenrollWorker", () => {
  it("posts to the host's own re-enroll route and returns the rotated token", async () => {
    const fetchMock = stubFetch(jsonResponse({ token: "rot8", expiresAtUnixMs: 99 }));
    const result = await reenrollWorker(7n);
    expect(fetchMock.mock.calls[0][0]).toBe("/api/workers/7/reenroll");
    expect((fetchMock.mock.calls[0][1] as RequestInit).method).toBe("POST");
    expect(result).toEqual({ token: "rot8", expiresAtUnixMs: 99 });
  });

  it("sends only the connection it is moving the host to", async () => {
    const fetchMock = stubFetch(jsonResponse({ token: "rot8", expiresAtUnixMs: 99 }));
    await reenrollWorker(7n, { connectMode: "accept", endpoint: "box:7677" });
    const body = JSON.parse((fetchMock.mock.calls[0][1] as RequestInit).body as string);
    expect(body).toEqual({ connect_mode: "accept", endpoint: "box:7677" });
  });

  it("surfaces the server error message", async () => {
    stubFetch(jsonResponse({ error: "the local host does not enroll" }, { status: 400 }));
    await expect(reenrollWorker(0n)).rejects.toThrow(
      "the local host does not enroll",
    );
  });
});

describe("removeWorker", () => {
  it("issues a DELETE for the worker", async () => {
    const fetchMock = stubFetch(new Response(null, { status: 204 }));
    await removeWorker(7n);
    expect(fetchMock.mock.calls[0][0]).toBe("/api/workers/7");
    expect((fetchMock.mock.calls[0][1] as RequestInit).method).toBe("DELETE");
  });
});

describe("setBucketWorker", () => {
  it("sends the worker id as a number", async () => {
    const fetchMock = stubFetch(new Response(null, { status: 204 }));
    await setBucketWorker(3n, 5n);
    expect(fetchMock.mock.calls[0][0]).toBe("/api/buckets/3/worker");
    expect(lastBody(fetchMock)).toEqual({ worker_id: 5 });
  });
});

describe("setProjectWorker", () => {
  it("sends the chosen worker id", async () => {
    const fetchMock = stubFetch(new Response(null, { status: 204 }));
    await setProjectWorker(9n, 2n);
    expect(fetchMock.mock.calls[0][0]).toBe("/api/projects/9/worker");
    expect(lastBody(fetchMock)).toEqual({ worker_id: 2 });
  });

  it("clears the override with a null worker id", async () => {
    const fetchMock = stubFetch(new Response(null, { status: 204 }));
    await setProjectWorker(9n, null);
    expect(lastBody(fetchMock)).toEqual({ worker_id: null });
  });
});

describe("platform gating", () => {
  /// The launcher never tries Incus off Linux, so offering it there
  /// would generate a command that machine cannot run.
  it("offers Incus on Linux only", () => {
    expect(runsOn("incus", "linux")).toBe(true);
    expect(runsOn("incus", "macos")).toBe(false);
  });

  it("offers everything else on both", () => {
    for (const type of ["machine", "docker", "podman", "lima"] as const) {
      expect(runsOn(type, "macos")).toBe(true);
      expect(runsOn(type, "linux")).toBe(true);
    }
  });

  /// A Lima VM is a guest of the controller's own machine, so a worker
  /// somewhere else is never offered it and never gets its host name.
  it("offers Lima for a local worker only", () => {
    expect(offeredAt("lima", "local")).toBe(true);
    expect(offeredAt("lima", "remote")).toBe(false);
    for (const type of ["machine", "docker", "podman", "incus"] as const) {
      expect(offeredAt(type, "remote")).toBe(true);
    }
  });

  it("falls back to the machine itself for a type that cannot be used there", () => {
    expect(resolveWorkerType("docker", "macos", "remote")).toBe("docker");
    expect(resolveWorkerType("incus", "macos", "local")).toBe("machine");
    expect(resolveWorkerType("lima", "macos", "remote")).toBe("machine");
  });

  it("reads the controller's own OS as the platform to start from", () => {
    expect(platformOf("macos")).toBe("macos");
    expect(platformOf("linux")).toBe("linux");
    // Anything else is a machine we have no snippets for, and Linux is
    // the one a worker is most likely to be.
    expect(platformOf("")).toBe("linux");
  });
});

describe("installSteps", () => {
  it("lists pm first, then the runtime that type needs", () => {
    expect(installSteps("docker", "macos", "curl pm | sh")).toEqual([
      "curl pm | sh",
      "brew install --cask docker && open -a Docker",
    ]);
    expect(installSteps("docker", "linux", "curl pm | sh")).toEqual([
      "curl pm | sh",
      "sudo apt install docker.io",
    ]);
  });

  it("lists pm alone for a type that needs no runtime", () => {
    expect(installSteps("machine", "linux", "curl pm | sh")).toEqual(["curl pm | sh"]);
    expect(installSteps("machine", "macos", "curl pm | sh")).toEqual(["curl pm | sh"]);
  });

  /// An older controller reports no install line, and an empty command
  /// is worse than no block at all.
  it("lists nothing when the controller reported no installer", () => {
    expect(installSteps("machine", "linux", "")).toEqual([]);
    expect(installSteps("docker", "linux", "")).toEqual(["sudo apt install docker.io"]);
  });

  /// Incus is Linux-only, so there is no macOS line to offer.
  it("has no Incus line for macOS", () => {
    expect(installSteps("incus", "macos", "curl pm | sh")).toEqual(["curl pm | sh"]);
  });
});
