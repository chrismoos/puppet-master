// Fetch adapters for the daemon's mobile device-auth endpoints. Every
// credential travels in a header or the JSON body; the only value that
// may ever reach a URL is the one-use socket ticket the daemon defines.

import type { JsonFetch } from "@puppet-master/client-core/platform";

export interface JsonResponseLike {
  status: number;
  json(): Promise<unknown>;
}

export type JsonFetchLike = (
  url: string,
  init: {
    method: string;
    headers: Record<string, string>;
    body?: string;
    signal?: AbortSignal;
  },
) => Promise<JsonResponseLike>;

/** HTTP-level failure from an auth endpoint, keeping the status code. */
export class AuthHttpError extends Error {
  constructor(
    readonly status: number,
    message: string,
  ) {
    super(message);
    this.name = "AuthHttpError";
  }
}

export interface MobileTokens {
  accessToken: string;
  accessTokenExpiresAtUnixMs: number;
  refreshToken: string;
  refreshTokenExpiresAtUnixMs: number;
}

export interface EnrolledDevice {
  id: string;
  name: string;
  platform: string;
}

export interface Enrollment {
  device: EnrolledDevice;
  tokens: MobileTokens;
  installationId: string | null;
}

export type EnrollProof =
  | { username: string; password: string }
  | { enrollToken: string };

export interface EnrollRequest {
  /** App-generated stable id for this installation. */
  deviceId: string;
  name: string;
  platform: string;
  proof: EnrollProof;
}

export interface SocketTicket {
  ticket: string;
  expiresAtUnixMs: number;
}

function origin(baseUrl: string): string {
  return baseUrl.replace(/\/+$/, "");
}

async function jsonRequest(
  fetchImpl: JsonFetchLike,
  method: string,
  url: string,
  body: unknown,
  accessToken?: string,
  signal?: AbortSignal,
): Promise<unknown> {
  const headers: Record<string, string> = {};
  if (body !== undefined) headers["Content-Type"] = "application/json";
  if (accessToken !== undefined) headers["Authorization"] = `Bearer ${accessToken}`;
  const response = await fetchImpl(url, {
    method,
    headers,
    body: body === undefined ? undefined : JSON.stringify(body),
    signal,
  });
  const payload = await response.json().catch(() => null);
  failUnlessOk(response.status, payload);
  return payload;
}

async function postJson(
  fetchImpl: JsonFetchLike,
  url: string,
  body: unknown,
  accessToken?: string,
  signal?: AbortSignal,
): Promise<unknown> {
  return jsonRequest(fetchImpl, "POST", url, body, accessToken, signal);
}

function failUnlessOk(status: number, payload: unknown): void {
  if (status >= 200 && status < 300) return;
  const detail =
    payload && typeof payload === "object" && typeof (payload as { error?: unknown }).error === "string"
      ? (payload as { error: string }).error
      : `request failed (${status})`;
  throw new AuthHttpError(status, detail);
}

function field(payload: unknown, key: string): unknown {
  if (typeof payload !== "object" || payload === null) return undefined;
  return (payload as Record<string, unknown>)[key];
}

function parseTokens(raw: unknown): MobileTokens {
  const accessToken = field(raw, "accessToken");
  const accessExpires = field(raw, "accessTokenExpiresAtUnixMs");
  const refreshToken = field(raw, "refreshToken");
  const refreshExpires = field(raw, "refreshTokenExpiresAtUnixMs");
  if (
    typeof accessToken !== "string" ||
    !accessToken ||
    typeof refreshToken !== "string" ||
    !refreshToken ||
    typeof accessExpires !== "number" ||
    typeof refreshExpires !== "number"
  ) {
    throw new Error("malformed token response");
  }
  return {
    accessToken,
    accessTokenExpiresAtUnixMs: accessExpires,
    refreshToken,
    refreshTokenExpiresAtUnixMs: refreshExpires,
  };
}

/** Registers this device and returns its first token pair. */
export async function enrollDevice(
  baseUrl: string,
  request: EnrollRequest,
  fetchImpl: JsonFetchLike,
): Promise<Enrollment> {
  const body: Record<string, unknown> = {
    deviceId: request.deviceId,
    name: request.name,
    platform: request.platform,
  };
  if ("enrollToken" in request.proof) {
    body["enrollToken"] = request.proof.enrollToken;
  } else {
    body["username"] = request.proof.username;
    body["password"] = request.proof.password;
  }
  const payload = await postJson(fetchImpl, `${origin(baseUrl)}/api/mobile/devices/enroll`, body);
  const device = field(payload, "device");
  const id = field(device, "id");
  const name = field(device, "name");
  const platform = field(device, "platform");
  if (typeof id !== "string" || typeof name !== "string" || typeof platform !== "string") {
    throw new Error("malformed enrollment response");
  }
  const installationId = field(payload, "installationId");
  return {
    device: { id, name, platform },
    tokens: parseTokens(field(payload, "tokens")),
    installationId: typeof installationId === "string" ? installationId : null,
  };
}

/** Rotates a refresh token into a fresh token pair. The device id comes
 * back so a client that lost it can recover without re-enrolling; a
 * daemon older than that field simply omits it. */
export async function refreshTokens(
  baseUrl: string,
  refreshToken: string,
  fetchImpl: JsonFetchLike,
  signal?: AbortSignal,
): Promise<{ tokens: MobileTokens; deviceId: string | null }> {
  const payload = await postJson(
    fetchImpl,
    `${origin(baseUrl)}/api/mobile/devices/refresh`,
    { refreshToken },
    undefined,
    signal,
  );
  const deviceId = field(payload, "deviceId");
  return {
    tokens: parseTokens(field(payload, "tokens")),
    deviceId: typeof deviceId === "string" && deviceId ? deviceId : null,
  };
}

function parseTicket(payload: unknown): SocketTicket {
  const ticket = field(payload, "ticket");
  const expiresAtUnixMs = field(payload, "expiresAtUnixMs");
  if (typeof ticket !== "string" || !ticket || typeof expiresAtUnixMs !== "number") {
    throw new Error("malformed ticket response");
  }
  return { ticket, expiresAtUnixMs };
}

/** Mints the one-use ticket a control `/ws` upgrade may consume. */
export async function mintControlSocketTicket(
  baseUrl: string,
  accessToken: string,
  fetchImpl: JsonFetchLike,
): Promise<SocketTicket> {
  const payload = await postJson(
    fetchImpl,
    `${origin(baseUrl)}/api/ws/ticket`,
    undefined,
    accessToken,
  );
  return parseTicket(payload);
}

/** Adapts the daemon fetch contract to a bearer-authenticated call so the
 *  shared client-core API helpers can run against the mobile session. */
export function bearerJsonFetch(
  baseUrl: string,
  accessToken: string,
  fetchImpl: JsonFetchLike,
): JsonFetch {
  return async (path, options) => {
    const response = await fetchImpl(`${origin(baseUrl)}${path}`, {
      method: options?.method ?? "GET",
      headers: { Authorization: `Bearer ${accessToken}` },
    });
    const payload = await response.json().catch(() => null);
    failUnlessOk(response.status, payload);
    return { ok: true, status: response.status, json: async () => payload };
  };
}

/**
 * Mints the one-use attach ticket a terminal WebSocket upgrade consumes,
 * bound to the terminal's current generation and a bounded replay size.
 */
export async function mintTerminalAttachTicket(
  baseUrl: string,
  accessToken: string,
  terminalId: string,
  generation: string,
  fetchImpl: JsonFetchLike,
  replayBytes?: number,
): Promise<SocketTicket> {
  const body: Record<string, unknown> = { generation };
  if (replayBytes !== undefined) body["replayBytes"] = replayBytes;
  const payload = await postJson(
    fetchImpl,
    `${origin(baseUrl)}/api/terminals/${terminalId}/attach-ticket`,
    body,
    accessToken,
  );
  return parseTicket(payload);
}

export interface PushRegistration {
  token: string;
  environment: string;
  locale: string;
  previewsEnabled: boolean;
  /** Base64-encoded X25519 public key for HPKE-sealed notifications. */
  publicKey?: string;
}

/** Registers or rotates this device's push endpoint on the controller. */
export async function registerPushEndpoint(
  baseUrl: string,
  accessToken: string,
  serverDeviceId: string,
  registration: PushRegistration,
  fetchImpl: JsonFetchLike,
): Promise<void> {
  await jsonRequest(
    fetchImpl,
    "PUT",
    `${origin(baseUrl)}/api/mobile/devices/${serverDeviceId}/push`,
    registration,
    accessToken,
  );
}
