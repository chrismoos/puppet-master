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

/**
 * The most of a token's life either lead may claim. A daemon configured
 * with a short access lifetime would otherwise hand out tokens that are
 * already inside a fixed lead, so the app would refresh the moment it
 * stored one and rotate without pause.
 */
const LEAD_SHARE_OF_LIFETIME = 4;

/** How long a caller waits for a refresh before it gives up. The request
 * itself is left to finish: the daemon accepts a refresh token once and
 * tolerates a single repeat, so handing it the same token again while the
 * first attempt is still outstanding is what costs the device its
 * credentials. */
const REFRESH_TIMEOUT_MS = 15_000;

const DEVICE_ID_BYTES = 16;

/** Which credential the daemon refused, which is what decides the fix. */
export type AuthStep = "refresh" | "access";

/** One refusal, kept so the user is told which credential failed and
 * what the daemon answered rather than only that something is wrong. */
export interface AuthFailure {
  step: AuthStep;
  /** The HTTP status, or 0 when the request never got an answer. */
  status: number;
  reason: string;
}

export type DeviceAuthStatus =
  | { kind: "unenrolled" }
  | { kind: "enrolled" }
  /** The daemon definitively refused the refresh token: the device was
   * revoked or the token family expired, so the user must re-enroll. */
  | { kind: "rejected"; reason: string; status: number; step: AuthStep };

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
  private failure: AuthFailure | null = null;
  private accessLifetimeMs = 0;
  private clockDisagreementMs = 0;
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

  /** The most recent refusal, kept after a failure the device can still
   * recover from so the connection header can say what went wrong. */
  lastFailure = (): AuthFailure | null => this.failure;

  /**
   * How far the daemon's clock is behind this device's, measured from an
   * access token that was already expiring when it arrived. Zero when
   * the two agree. A disagreement makes every token look spent on
   * arrival, so the app refreshes without pause and nothing else it
   * reports makes sense until it is fixed.
   */
  clockDisagreement = (): number => this.clockDisagreementMs;

  /**
   * Records that the daemon refused the access token this device holds,
   * which it reports by closing a socket rather than by answering a
   * request.
   */
  accessRefused(): void {
    this.noteFailure({
      step: "access",
      status: 401,
      reason: "the daemon refused this device's access token",
    });
  }

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
    if (token && this.now() < expiresAt - this.lead(ACCESS_EXPIRY_SLACK_MS)) return token;
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
    const refreshAt = expiresAt - this.lead(PROACTIVE_REFRESH_SLACK_MS);
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

  /**
   * Rotates the refresh token now; used when the daemon reported the
   * current access token invalid. One rotation is outstanding at a time,
   * and a caller that stops waiting joins that one instead of starting
   * another: a second presentation of a token the daemon is already
   * rotating spends the one repeat it tolerates, and the next revokes
   * the device.
   */
  refresh(baseUrl: string): Promise<MobileTokens> {
    if (!this.refreshInFlight) {
      const rotating = this.rotate(baseUrl);
      this.refreshInFlight = rotating;
      void rotating
        .catch(() => {})
        .finally(() => {
          if (this.refreshInFlight === rotating) this.refreshInFlight = null;
        });
    }
    return withTimeout(this.refreshInFlight, this.refreshTimeoutMs).catch((err: unknown) => {
      if (err instanceof RefreshTimeout) {
        this.noteFailure({
          step: "refresh",
          status: 0,
          reason: `the daemon did not answer within ${Math.round(this.refreshTimeoutMs / 1000)}s`,
        });
      }
      throw err;
    });
  }

  private async rotate(baseUrl: string): Promise<MobileTokens> {
    const refreshToken = this.storage.getItem(AUTH_KEYS.refreshToken);
    if (!refreshToken) {
      throw new AuthRejectedError("this device is not enrolled");
    }
    let tokens: MobileTokens;
    let refreshedDeviceId: string | null = null;
    try {
      const refreshed = await refreshTokens(baseUrl, refreshToken, this.fetchImpl);
      tokens = refreshed.tokens;
      refreshedDeviceId = refreshed.deviceId;
    } catch (err) {
      // Only the daemon's own refusal is a verdict on this device. A 401
      // from a proxy, a gateway or a network standing in for one says
      // nothing about the credential, and dropping it on one of those
      // makes the user enroll again for a stranger's answer.
      if (err instanceof AuthHttpError && err.status === 401 && err.fromDaemon) {
        this.clearTokens();
        // Set before the status, which is the one notification listeners
        // need: the banner reads both.
        this.failure = { step: "refresh", status: err.status, reason: err.message };
        this.setStatus({
          kind: "rejected",
          reason: err.message,
          status: err.status,
          step: "refresh",
        });
        throw new AuthRejectedError(err.message);
      }
      if (err instanceof AuthHttpError) {
        this.noteFailure({ step: "refresh", status: err.status, reason: err.message });
      } else {
        this.noteFailure({
          step: "refresh",
          status: 0,
          reason: err instanceof Error ? err.message : "the refresh failed",
        });
      }
      // Network or server failure: the stored tokens stay valid and the
      // daemon has not rotated them, so the caller may retry later.
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
    // An access token that is already inside the expiry slack when it
    // arrives cannot have come from a clock this device agrees with, and
    // the app would refresh again at once, rotating without pause.
    this.accessLifetimeMs = Math.max(0, tokens.accessTokenExpiresAtUnixMs - this.now());
    const short = this.now() - (tokens.accessTokenExpiresAtUnixMs - this.lead(ACCESS_EXPIRY_SLACK_MS));
    const disagreement = short > 0 ? short : 0;
    const changed = disagreement !== this.clockDisagreementMs || this.failure !== null;
    this.clockDisagreementMs = disagreement;
    this.failure = null;
    if (changed) this.notify();
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

  private noteFailure(failure: AuthFailure | null): void {
    const was = this.failure;
    if (
      was === failure ||
      (was &&
        failure &&
        was.step === failure.step &&
        was.status === failure.status &&
        was.reason === failure.reason)
    ) {
      return;
    }
    this.failure = failure;
    this.notify();
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
    this.notify();
  }

  /// Bounded by the lifetime the daemon actually issues, which is a
  /// setting and can be shorter than the lead this would otherwise take.
  private lead(preferredMs: number): number {
    if (this.accessLifetimeMs <= 0) return preferredMs;
    return Math.min(preferredMs, Math.floor(this.accessLifetimeMs / LEAD_SHARE_OF_LIFETIME));
  }

  private notify(): void {
    for (const listener of this.listeners) listener();
  }
}

/** The caller gave up waiting; the request itself is still outstanding. */
class RefreshTimeout extends Error {
  constructor() {
    super("refresh request timed out");
    this.name = "RefreshTimeout";
  }
}

function withTimeout<T>(promise: Promise<T>, ms: number): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const timer = setTimeout(() => reject(new RefreshTimeout()), ms);
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
