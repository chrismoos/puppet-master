import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { forgetAccessToken } from "../api/token";
import { seedAccessToken } from "../api/token.fixture";
import { mintSocketTicket, ticketRequestFor, withTicket } from "./socketTicket";

describe("ticketRequestFor", () => {
  it("sends the control socket to its own mint", () => {
    expect(ticketRequestFor("/ws")).toEqual({ path: "/api/ws/ticket" });
  });

  /// A terminal ticket is bound to the generation it was minted against, so the
  /// generation the socket is about to ask for is the one the mint must name.
  it("names the terminal and the generation the socket will ask for", () => {
    expect(ticketRequestFor("/ws/terminal/7?generation=3&cols=80&rows=24")).toEqual({
      path: "/api/terminals/7/attach-ticket",
      body: JSON.stringify({ generation: 3 }),
    });
  });

  it("needs a generation, because a ticket cannot be bound without one", () => {
    expect(ticketRequestFor("/ws/terminal/7")).toBeNull();
    expect(ticketRequestFor("/ws/terminal/7?generation=later")).toBeNull();
  });

  it("asks for no ticket on a path it does not know", () => {
    expect(ticketRequestFor("/ws/other")).toBeNull();
    expect(ticketRequestFor("/api/me")).toBeNull();
  });
});

describe("withTicket", () => {
  it("appends to a path that already carries a query", () => {
    expect(withTicket("/ws/terminal/7?generation=3", "abc")).toBe(
      "/ws/terminal/7?generation=3&ticket=abc",
    );
  });

  it("starts the query on a path without one, and escapes the value", () => {
    expect(withTicket("/ws", "a b&c")).toBe("/ws?ticket=a%20b%26c");
  });
});

describe("mintSocketTicket", () => {
  beforeEach(seedAccessToken);
  afterEach(() => {
    vi.unstubAllGlobals();
    forgetAccessToken();
  });

  it("spends the access token on a ticket for the control socket", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(new Response(JSON.stringify({ ticket: "one-use" }), { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);

    expect(await mintSocketTicket("/ws")).toBe("one-use");
    const [path, init] = fetchMock.mock.calls[0];
    expect(path).toBe("/api/ws/ticket");
    expect(new Headers(init?.headers).get("Authorization")).toBe("Bearer seeded-test-token");
  });

  it("tells the terminal mint which generation it is for", async () => {
    const fetchMock = vi
      .fn()
      .mockResolvedValue(new Response(JSON.stringify({ ticket: "bound" }), { status: 200 }));
    vi.stubGlobal("fetch", fetchMock);

    await mintSocketTicket("/ws/terminal/7?generation=3");

    const [path, init] = fetchMock.mock.calls[0];
    expect(path).toBe("/api/terminals/7/attach-ticket");
    expect(init?.body).toBe(JSON.stringify({ generation: 3 }));
  });

  it("fails loudly rather than opening a socket with no ticket", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response("", { status: 404 })));

    await expect(mintSocketTicket("/ws")).rejects.toThrow(/could not mint a socket ticket/);
  });

  it("asks for nothing on a path that needs no ticket", async () => {
    const fetchMock = vi.fn();
    vi.stubGlobal("fetch", fetchMock);

    expect(await mintSocketTicket("/ws/unknown")).toBeNull();
    expect(fetchMock).not.toHaveBeenCalled();
  });
});
