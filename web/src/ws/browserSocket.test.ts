import { afterEach, describe, expect, it, vi } from "vitest";
import ghosttyRevision from "./ghostty/vendor/VERSION?raw";
import { browserSocketConnector } from "./browserSocket";
import {
  TERMINAL_CLOSE_INCOMPATIBLE,
  TERMINAL_CLOSE_PAGE_OUTDATED,
} from "@puppet-master/client-core/ws/terminalFrame";
import { authedFetch } from "../api/token";
import { mintSocketTicket } from "./socketTicket";

vi.mock("../api/token", () => ({
  authedFetch: vi.fn(), UnauthenticatedError: class extends Error {},
}));
vi.mock("./socketTicket", () => ({
  mintSocketTicket: vi.fn(), withTicket: (path: string) => `${path}&ticket=one-use`,
}));

afterEach(() => { vi.unstubAllGlobals(); vi.clearAllMocks(); });

function environment() {
  vi.stubGlobal("location", { protocol: "https:", host: "preview.invalid" });
  const opened = vi.fn(function (_url: string, _protocol?: string) { return { binaryType: "", readyState: 0 }; });
  vi.stubGlobal("WebSocket", opened);
  vi.mocked(mintSocketTicket).mockResolvedValue("one-use");
  return opened;
}

describe("browser terminal snapshot negotiation", () => {
  it("checks the pinned engine before opening a binary snapshot socket", async () => {
    const opened = environment();
    vi.mocked(authedFetch).mockResolvedValue(new Response("{}"));
    browserSocketConnector("/ws/terminal/7?generation=3");
    await vi.waitFor(() => expect(opened).toHaveBeenCalledOnce());
    const identity = encodeURIComponent(`ghostty-vt:${ghosttyRevision.trim()}:pm-v1`);
    expect(authedFetch).toHaveBeenCalledWith(`/api/terminals/7/snapshot-compatibility?ghostty=${identity}`);
    expect(opened.mock.calls[0]?.[0]).toContain(`&ghostty=${identity}&ticket=one-use`);
  });

  it("reports an incompatible engine without opening or minting a terminal socket", async () => {
    const opened = environment();
    vi.mocked(authedFetch).mockResolvedValue(new Response("Reload to update the terminal engine", { status: 409 }));
    const socket = browserSocketConnector("/ws/terminal/7?generation=3");
    const closed = vi.fn();
    socket.onclose = closed;
    await vi.waitFor(() => expect(closed).toHaveBeenCalledWith(expect.objectContaining({ reason: "Reload to update the terminal engine" })));
    expect(opened).not.toHaveBeenCalled();
    expect(mintSocketTicket).not.toHaveBeenCalled();
  });

  it("closes with the page-outdated code when the controller says a reload fixes it", async () => {
    environment();
    vi.mocked(authedFetch).mockResolvedValue(new Response("Reload this page", {
      status: 409, headers: { "x-pm-terminal-incompatible": "page" },
    }));
    const socket = browserSocketConnector("/ws/terminal/7?generation=3");
    const closed = vi.fn();
    socket.onclose = closed;
    await vi.waitFor(() => expect(closed).toHaveBeenCalledWith(expect.objectContaining({ code: TERMINAL_CLOSE_PAGE_OUTDATED })));
  });

  it("closes with the incompatible code when the worker is out of date", async () => {
    environment();
    vi.mocked(authedFetch).mockResolvedValue(new Response("Update this worker", {
      status: 409, headers: { "x-pm-terminal-incompatible": "worker" },
    }));
    const socket = browserSocketConnector("/ws/terminal/7?generation=3");
    const closed = vi.fn();
    socket.onclose = closed;
    await vi.waitFor(() => expect(closed).toHaveBeenCalledWith(expect.objectContaining({ code: TERMINAL_CLOSE_INCOMPATIBLE })));
  });

  it("leaves control sockets independent of the terminal engine", async () => {
    const opened = environment();
    browserSocketConnector("/ws");
    await vi.waitFor(() => expect(opened).toHaveBeenCalledOnce());
    expect(authedFetch).not.toHaveBeenCalled();
  });
});
