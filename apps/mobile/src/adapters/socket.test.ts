import { describe, expect, it } from "vitest";

import type { SocketLike } from "@puppet-master/client-core/platform";

import { createSocketConnector, websocketBaseUrl, type SocketFactoryOptions } from "./socket";

interface FactoryCall {
  url: string;
  subprotocol: string | undefined;
  options: SocketFactoryOptions | undefined;
}

function captureFactory(calls: FactoryCall[]) {
  return (url: string, subprotocol: string | undefined, options: SocketFactoryOptions | undefined): SocketLike => {
    calls.push({ url, subprotocol, options });
    return {} as SocketLike;
  };
}

describe("websocketBaseUrl", () => {
  it("maps http schemes to websocket schemes and trims trailing slashes", () => {
    expect(websocketBaseUrl("https://pm.example")).toBe("wss://pm.example");
    expect(websocketBaseUrl("http://192.168.1.10:8080/")).toBe("ws://192.168.1.10:8080");
    expect(websocketBaseUrl("HTTPS://pm.example//")).toBe("wss://pm.example");
  });

  it("rejects non-http URLs", () => {
    expect(() => websocketBaseUrl("ftp://pm.example")).toThrow();
    expect(() => websocketBaseUrl("pm.example")).toThrow();
  });
});

describe("createSocketConnector", () => {
  it("joins the daemon path and forwards the subprotocol", () => {
    const calls: FactoryCall[] = [];
    const connector = createSocketConnector(
      () => ({ baseUrl: "https://pm.example/" }),
      captureFactory(calls),
    );
    connector("/ws/terminal/7?generation=3", "pm-terminal-v1");
    expect(calls).toEqual([
      {
        url: "wss://pm.example/ws/terminal/7?generation=3",
        subprotocol: "pm-terminal-v1",
        options: undefined,
      },
    ]);
  });

  it("sends the bearer access token as an Authorization header, never in the URL", () => {
    const calls: FactoryCall[] = [];
    const connector = createSocketConnector(
      () => ({ baseUrl: "http://10.0.0.5:8080", accessToken: "acc123" }),
      captureFactory(calls),
    );
    connector("/ws");
    expect(calls[0].url).toBe("ws://10.0.0.5:8080/ws");
    expect(calls[0].url).not.toContain("acc123");
    expect(calls[0].options).toEqual({ headers: { Authorization: "Bearer acc123" } });
  });

  /// The daemon stopped reading the session cookie on a socket upgrade, so a
  /// lingering one authenticates nothing and is not worth sending.
  it("sends no credential at all when there is no bearer token", () => {
    const calls: FactoryCall[] = [];
    const connector = createSocketConnector(
      () => ({ baseUrl: "http://10.0.0.5:8080", accessToken: null, sessionToken: "tok123" }),
      captureFactory(calls),
    );
    connector("/ws");
    expect(calls[0].url).not.toContain("tok123");
    expect(calls[0].options).toBeUndefined();
  });

  it("ignores a lingering legacy cookie token in favour of the bearer", () => {
    const calls: FactoryCall[] = [];
    const connector = createSocketConnector(
      () => ({ baseUrl: "https://pm.example", accessToken: "acc123", sessionToken: "tok123" }),
      captureFactory(calls),
    );
    connector("/ws");
    expect(calls[0].options).toEqual({ headers: { Authorization: "Bearer acc123" } });
  });

  it("re-reads the endpoint on every connection", () => {
    const calls: FactoryCall[] = [];
    let token: string | null = null;
    const connector = createSocketConnector(
      () => ({ baseUrl: "https://pm.example", accessToken: token }),
      captureFactory(calls),
    );
    connector("/ws");
    token = "fresh";
    connector("/ws");
    expect(calls[0].options).toBeUndefined();
    expect(calls[1].options).toEqual({ headers: { Authorization: "Bearer fresh" } });
  });
});
