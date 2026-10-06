import type { KeyValueStorage } from "@puppet-master/client-core/platform";

import {
  AuthHttpError,
  enrollDevice,
  refreshTokens,
  type EnrollProof,
  type JsonFetchLike,
  type MobileTokens,
} from "./api";

export const AUTH_KEYS = {
  deviceId: "auth.deviceId",
  serverDeviceId: "auth.serverDeviceId",
  accessToken: "auth.accessToken",
  accessExpiresAt: "auth.accessTokenExpiresAtUnixMs",
  refreshToken: "auth.refreshToken",
  refreshExpiresAt: "auth.refreshTokenExpiresAtUnixMs",
  deviceName: "auth.deviceName",
  enrolledAt: "auth.enrolledAtUnixMs",
} as const;

export const ALL_AUTH_KEYS: readonly string[] = Object.values(AUTH_KEYS);

/** Refresh this long before the access token's stated expiry. */
const ACCESS_EXPIRY_SLACK_MS = 30_000;

/** Proactive refresh fires this long before expiry so an open never waits. */
const PROACTIVE_REFRESH_SLACK_MS = 60_000;

/** A suspended or dead refresh request surfaces as a network error
 * after this many milliseconds so the stored tokens stay valid. */
const REFRESH_TIMEOUT_MS = 15_000;

const DEVICE_ID_BYTES = 16;

export type DeviceAuthStatus =
  | { kind: "unenrolled" }
  | { kind: "enrolled" }
  /** The daemon definitively refused the refresh token: the device was
   * revoked or the token family expired, so the user must re-enroll. */
  | { kind: "rejected"; reason: string };

/** Raised when authentication cannot recover without re-enrolling. */
export class AuthRejectedError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "AuthRejectedError";
  }
}

export function generateDeviceId(): string {
  const bytes = new Uint8Array(DEVICE_ID_BYTES);
  const cryptoLike = (globalThis as { crypto?: { getRandomValues?(array: Uint8Array): Uint8Array } })
    .crypto;
  if (cryptoLike?.getRandomValues) {
    cryptoLike.getRandomValues(bytes);
  } else {
    // The device id is an identifier, not a secret; a weak source only
    // risks an id collision, which enrollment tolerates.
    for (let i = 0; i < bytes.length; i += 1) bytes[i] = Math.floor(Math.random() * 256);
  }
  return [...bytes].map((b) => b.toString(16).padStart(2, "0")).join("");
}

/**
 * Holds this installation's device credentials: the stable app-generated
 * device id and the bearer access + rotating refresh tokens, persisted only
 * through the OS secure store. Refreshes are single-flight, and a refresh
 * the daemon definitively refuses moves the session to a visible
 * "rejected" state instead of retrying forever.
 */
export class DeviceAuthSession {
  private statusValue: DeviceAuthStatus;
  private listeners = new Set<() => void>();
  private refreshInFlight: Promise<MobileTokens> | null = null;
  private refreshTimeoutMs: number;
  private proactiveTimer: ReturnType<typeof setTimeout> | null = null;
  private proactiveBaseUrl: string | null = null;

  constructor(
    private storage: KeyValueStorage,
    private fetchImpl: JsonFetchLike,
    private now: () => number = Date.now,
    refreshTimeoutMs?: number,
  ) {
    this.refreshTimeoutMs = refreshTimeoutMs ?? REFRESH_TIMEOUT_MS;
    this.statusValue = this.storage.getItem(AUTH_KEYS.refreshToken)
      ? { kind: "enrolled" }
      : { kind: "unenrolled" };
  }

  status = (): DeviceAuthStatus => this.statusValue;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  /** The stable app-generated device id, created on first use. */
  deviceId(): string {
    const existing = this.storage.getItem(AUTH_KEYS.deviceId);
    if (existing) return existing;
    const generated = generateDeviceId();
    this.storage.setItem(AUTH_KEYS.deviceId, generated);
    return generated;
  }

  /** The server-assigned numeric device id, set at enrollment. */
  serverDeviceId(): string | null {
    return this.storage.getItem(AUTH_KEYS.serverDeviceId) || null;
  }

  /** The current access token for connection headers; possibly stale,
   * the daemon stays the authority on validity. */
  accessToken(): string | null {
    return this.storage.getItem(AUTH_KEYS.accessToken) || null;
  }

  async enroll(
    baseUrl: string,
    proof: EnrollProof,
    meta: { name: string; platform: string },
  ): Promise<void> {
    const enrollment = await enrollDevice(
      baseUrl,
      { deviceId: this.deviceId(), name: meta.name, platform: meta.platform, proof },
      this.fetchImpl,
    );
    this.storage.setItem(AUTH_KEYS.serverDeviceId, enrollment.device.id);
    this.storeTokens(enrollment.tokens);
    this.storage.setItem(AUTH_KEYS.deviceName, meta.name);
    this.storage.setItem(AUTH_KEYS.enrolledAt, String(this.now()));
    this.setStatus({ kind: "enrolled" });
  }

  /** The human-readable device name recorded at enrolment. */
  deviceName(): string | null {
    return this.storage.getItem(AUTH_KEYS.deviceName) || null;
  }

  /** Unix-ms timestamp of the most recent enrolment. */
  enrolledAt(): number | null {
    const raw = this.storage.getItem(AUTH_KEYS.enrolledAt);
    return raw ? Number(raw) : null;
  }

  /**
   * Returns an access token valid for at least the expiry slack,
   * rotating through the refresh endpoint when needed. Throws
   * AuthRejectedError once the daemon refuses the refresh token.
   */
  async freshAccessToken(baseUrl: string): Promise<string> {
    const token = this.accessToken();
    const expiresAt = Number(this.storage.getItem(AUTH_KEYS.accessExpiresAt) ?? "0");
    if (token && this.now() < expiresAt - ACCESS_EXPIRY_SLACK_MS) return token;
    const rotated = await this.refresh(baseUrl);
    return rotated.accessToken;
  }

  /** Start a timer that refreshes the access token about a minute before
   *  expiry so an open never waits on a refresh round trip. Call this when
   *  the app enters the foreground and stop it on background. */
  startProactiveRefresh(baseUrl: string): void {
    this.proactiveBaseUrl = baseUrl;
    this.scheduleProactiveRefresh();
  }

  stopProactiveRefresh(): void {
    if (this.proactiveTimer !== null) {
      clearTimeout(this.proactiveTimer);
      this.proactiveTimer = null;
    }
    this.proactiveBaseUrl = null;
  }

  private scheduleProactiveRefresh(): void {
    if (this.proactiveTimer !== null) clearTimeout(this.proactiveTimer);
    this.proactiveTimer = null;
    const baseUrl = this.proactiveBaseUrl;
    if (!baseUrl) return;
    const expiresAt = Number(this.storage.getItem(AUTH_KEYS.accessExpiresAt) ?? "0");
    if (expiresAt === 0) return;
    const refreshAt = expiresAt - PROACTIVE_REFRESH_SLACK_MS;
    const delay = Math.max(0, refreshAt - this.now());
    this.proactiveTimer = setTimeout(() => {
      this.proactiveTimer = null;
      if (!this.proactiveBaseUrl) return;
      this.refresh(this.proactiveBaseUrl)
        .then(() => this.scheduleProactiveRefresh())
        .catch(() => {
          // Network failure: retry in 10 s; the lazy path is the fallback.
          if (this.proactiveBaseUrl) {
            this.proactiveTimer = setTimeout(
              () => this.scheduleProactiveRefresh(),
              10_000,
            );
          }
        });
    }, delay);
  }

  /**
   * Runs an authenticated call, retrying once through a token refresh
   * when the daemon answers 401.
   */
  async withAccessToken<T>(baseUrl: string, call: (accessToken: string) => Promise<T>): Promise<T> {
    const token = await this.freshAccessToken(baseUrl);
    try {
      return await call(token);
    } catch (err) {
      if (!(err instanceof AuthHttpError) || err.status !== 401) throw err;
      const rotated = await this.refresh(baseUrl);
      return await call(rotated.accessToken);
    }
  }

  /** Rotates the refresh token now; used when the daemon reported the
   * current access token invalid. Single-flight across callers. */
  refresh(baseUrl: string): Promise<MobileTokens> {
    if (this.refreshInFlight) return this.refreshInFlight;
    const flight = this.rotate(baseUrl).finally(() => {
      this.refreshInFlight = null;
    });
    this.refreshInFlight = flight;
    return flight;
  }

  private async rotate(baseUrl: string): Promise<MobileTokens> {
    const refreshToken = this.storage.getItem(AUTH_KEYS.refreshToken);
    if (!refreshToken) {
      throw new AuthRejectedError("this device is not enrolled");
    }
    let tokens: MobileTokens;
    let refreshedDeviceId: string | null = null;
    try {
      const abort = new AbortController();
      const refreshed = await withTimeout(
        refreshTokens(baseUrl, refreshToken, this.fetchImpl, abort.signal),
        this.refreshTimeoutMs,
        abort,
      );
      tokens = refreshed.tokens;
      refreshedDeviceId = refreshed.deviceId;
    } catch (err) {
      if (err instanceof AuthHttpError && err.status === 401) {
        this.clearTokens();
        this.setStatus({ kind: "rejected", reason: err.message });
        throw new AuthRejectedError(err.message);
      }
      // Network, server, or timeout failure: the stored tokens stay
      // valid, the caller may retry later.
      throw err;
    }
    this.storeTokens(tokens);
    await this.flushStorage();
    // An enrolled device that lost its server id would report enrolled
    // while every call needing that id silently refused. Recover it from
    // the refresh rather than making the user enroll again.
    if (refreshedDeviceId && !this.serverDeviceId()) {
      this.storage.setItem(AUTH_KEYS.serverDeviceId, refreshedDeviceId);
    }
    this.setStatus({ kind: "enrolled" });
    return tokens;
  }

  /** Forgets the stored tokens; the device id is kept so re-enrolling
   * reuses the same installation identity. */
  signOut(): void {
    this.clearTokens();
    this.setStatus({ kind: "unenrolled" });
  }

  private storeTokens(tokens: MobileTokens): void {
    // Write the refresh token and its expiry first so a kill between
    // writes leaves the keychain with the token the daemon will accept
    // on the next retry.
    this.storage.setItem(AUTH_KEYS.refreshToken, tokens.refreshToken);
    this.storage.setItem(AUTH_KEYS.refreshExpiresAt, String(tokens.refreshTokenExpiresAtUnixMs));
    this.storage.setItem(AUTH_KEYS.accessToken, tokens.accessToken);
    this.storage.setItem(AUTH_KEYS.accessExpiresAt, String(tokens.accessTokenExpiresAtUnixMs));
    // Reschedule proactive refresh with the new expiry.
    if (this.proactiveBaseUrl) this.scheduleProactiveRefresh();
  }

  private async flushStorage(): Promise<void> {
    const s = this.storage as { flush?: () => Promise<void> };
    if (s.flush) await s.flush();
  }

  private clearTokens(): void {
    this.storage.setItem(AUTH_KEYS.serverDeviceId, "");
    this.storage.setItem(AUTH_KEYS.accessToken, "");
    this.storage.setItem(AUTH_KEYS.accessExpiresAt, "");
    this.storage.setItem(AUTH_KEYS.refreshToken, "");
    this.storage.setItem(AUTH_KEYS.refreshExpiresAt, "");
  }

  private setStatus(status: DeviceAuthStatus): void {
    if (
      status.kind === this.statusValue.kind &&
      (status.kind !== "rejected" ||
        (this.statusValue.kind === "rejected" && this.statusValue.reason === status.reason))
    ) {
      return;
    }
    this.statusValue = status;
    for (const listener of this.listeners) listener();
  }
}

function withTimeout<T>(promise: Promise<T>, ms: number, abort?: AbortController): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => {
      abort?.abort();
      reject(new Error("refresh request timed out"));
    }, ms);
    promise.then(
      (v) => {
        clearTimeout(timer);
        resolve(v);
      },
      (e: unknown) => {
        clearTimeout(timer);
        reject(e);
      },
    );
  });
}
