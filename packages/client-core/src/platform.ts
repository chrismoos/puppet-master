// Adapter contracts each host platform implements: the web client binds
// browser WebSocket/fetch/storage, a mobile host binds its own equivalents.

export interface SocketMessageEvent {
  data: unknown;
}

export interface SocketCloseEvent {
  code: number;
  reason: string;
}

/** The subset of the WHATWG WebSocket surface the client logic drives. */
export interface SocketLike {
  binaryType: string;
  readonly readyState: number;
  send(data: ArrayBufferLike | Uint8Array): void;
  close(): void;
  onopen: (() => void) | null;
  onmessage: ((event: SocketMessageEvent) => void) | null;
  onerror: (() => void) | null;
  onclose: ((event: SocketCloseEvent) => void) | null;
}

/** WHATWG WebSocket OPEN ready state. */
export const SOCKET_OPEN = 1;

/**
 * Opens a socket for a daemon path like "/ws" or "/ws/terminal/7". The
 * adapter owns scheme, host, and authentication.
 */
export type SocketConnector = (path: string, subprotocol?: string) => SocketLike;

export interface AbortSignalLike {
  readonly aborted: boolean;
}

export interface RequestOptions {
  method?: string;
  signal?: AbortSignalLike;
}

export interface JsonResponse {
  readonly ok: boolean;
  readonly status: number;
  json(): Promise<unknown>;
}

/**
 * Issues an HTTP request for a daemon path like "/api/buckets/1/items".
 * The adapter owns the base URL and bearer/cookie authentication.
 */
export type JsonFetch = (path: string, options?: RequestOptions) => Promise<JsonResponse>;

/** The subset of DOM Storage the shared state helpers persist through. */
export interface KeyValueStorage {
  getItem(key: string): string | null;
  setItem(key: string, value: string): void;
}
