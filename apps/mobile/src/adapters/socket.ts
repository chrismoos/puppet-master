import type { SocketConnector, SocketLike } from "@puppet-master/client-core/platform";

export interface SocketFactoryOptions {
  headers?: Record<string, string>;
}

/** React Native's WebSocket accepts a third options argument with headers. */
export type WebSocketFactory = (
  url: string,
  subprotocol: string | undefined,
  options: SocketFactoryOptions | undefined,
) => SocketLike;

export interface ControllerEndpoint {
  /** Canonical http(s) origin, e.g. "https://pm.example". */
  baseUrl: string;
  /** Mobile device bearer access token, sent as an Authorization header,
   * never embedded in the URL. */
  accessToken?: string | null;
  /** Retained so a config written by an older build still parses. Nothing
   * authenticates with it: the daemon stopped reading the session cookie on a
   * socket upgrade, so a device holding only this has to enrol again. */
  sessionToken?: string | null;
}

export function websocketBaseUrl(baseUrl: string): string {
  const trimmed = baseUrl.replace(/\/+$/, "");
  if (/^https:\/\//i.test(trimmed)) return trimmed.replace(/^https:\/\//i, "wss://");
  if (/^http:\/\//i.test(trimmed)) return trimmed.replace(/^http:\/\//i, "ws://");
  throw new Error("controller URL must start with http:// or https://");
}

/**
 * Daemon WebSocket adapter for React Native: derives ws(s) URLs from the
 * configured controller origin and authenticates with the device bearer token.
 * The endpoint is read per connection so reconnects pick up credential changes.
 */
export function createSocketConnector(
  endpoint: () => ControllerEndpoint,
  factory: WebSocketFactory,
): SocketConnector {
  return (path, subprotocol) => {
    const { baseUrl, accessToken } = endpoint();
    const url = websocketBaseUrl(baseUrl) + path;
    const options: SocketFactoryOptions | undefined = accessToken
      ? { headers: { Authorization: `Bearer ${accessToken}` } }
      : undefined;
    return factory(url, subprotocol, options);
  };
}

interface HeaderCapableWebSocket {
  new (
    url: string,
    protocols?: string | string[] | null,
    options?: SocketFactoryOptions | null,
  ): SocketLike;
}

/** Binds the global React Native WebSocket, which supports header options. */
export const reactNativeWebSocketFactory: WebSocketFactory = (url, subprotocol, options) => {
  const WebSocketCtor = (globalThis as { WebSocket?: unknown }).WebSocket as HeaderCapableWebSocket;
  return new WebSocketCtor(url, subprotocol ? [subprotocol] : undefined, options ?? undefined);
};
