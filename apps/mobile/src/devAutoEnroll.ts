/**
 * Automated enrollment for UI-test builds.
 *
 * Activated only when PM_UI_TEST_BUILD=true, PM_DEV_CONTROLLER_URL, and
 * PM_DEV_ENROLL_TOKEN are present in the Expo Constants manifest extra.
 * These are set by PM_UI_TEST_BUILD=1 at prebuild/xcodebuild time, with
 * the values coming from scripts/controller-fixture.mjs's PM_FIXTURE_*
 * output (see app.config.js).
 *
 * The enrollment token is single-use. Enrollment is idempotent by
 * fixture identity: if the persisted controller URL matches and auth is
 * enrolled, the existing credentials are reused. A URL change clears
 * stale state and enrolls with the fresh token. A consumed token is
 * never retried on a same-fixture relaunch.
 *
 * Normal development, ios-device, TestFlight, and release builds never
 * set the manifest flag, so this code is inert in those binaries.
 */

import type { DeviceAuthSession } from "./auth/session";
import type { CachedSecureStorage } from "./adapters/storage";
import { readControllerConfig, writeControllerConfig } from "./config";

export interface DevAutoEnrollEnv {
  controllerUrl: string;
  enrollToken: string;
}

/**
 * Pure extraction: given the manifest extra object, return an enroll env
 * if the UI-test flag, controller URL, and enrollment token are all present.
 */
export function extractEnrollEnv(
  extra: Record<string, unknown>,
): DevAutoEnrollEnv | null {
  if (extra.PM_UI_TEST_BUILD !== true) return null;
  const url = extra.PM_DEV_CONTROLLER_URL;
  const token = extra.PM_DEV_ENROLL_TOKEN;
  if (typeof url !== "string" || !url) return null;
  if (typeof token !== "string" || !token) return null;
  return { controllerUrl: url, enrollToken: token };
}

/**
 * Reads auto-enroll config from Expo Constants manifest extra.
 * Returns null in normal builds (no PM_UI_TEST_BUILD flag).
 */
export function readDevEnrollEnv(): DevAutoEnrollEnv | null {
  try {
    // eslint-disable-next-line @typescript-eslint/no-var-requires
    const Constants = require("expo-constants").default;
    return extractEnrollEnv(Constants?.expoConfig?.extra ?? {});
  } catch {
    return null;
  }
}

/**
 * Idempotent enrollment by fixture identity.
 *
 * - If the persisted controller URL matches `env.controllerUrl` and
 *   auth status is "enrolled", reuse the persisted credentials.
 * - If the URL differs or auth is not enrolled, clear the stale
 *   controller/auth state and enroll once with the fresh token.
 * - Never retry a consumed token on a same-fixture relaunch.
 */
export async function devAutoEnroll(
  env: DevAutoEnrollEnv,
  auth: DeviceAuthSession,
  storage: CachedSecureStorage,
): Promise<boolean> {
  const persisted = readControllerConfig(storage);
  const isEnrolled = auth.status().kind === "enrolled";

  // Same fixture, already enrolled: reuse credentials.
  if (isEnrolled && persisted?.baseUrl === env.controllerUrl) {
    console.log(`[ui-test-enroll] reusing enrollment for ${env.controllerUrl}`);
    return true;
  }

  // Different fixture or not enrolled: enroll with the fresh token.
  // Clear stale state first so a failed enrollment leaves a clean slate.
  if (persisted && persisted.baseUrl !== env.controllerUrl) {
    console.log(`[ui-test-enroll] controller changed, clearing stale state`);
    auth.signOut();
  }

  try {
    await auth.enroll(
      env.controllerUrl,
      { enrollToken: env.enrollToken },
      { name: "UITest Simulator", platform: "ios" },
    );
    writeControllerConfig(storage, {
      baseUrl: env.controllerUrl,
      sessionToken: null,
      pushPreviewsEnabled: false,
    });
    console.log(`[ui-test-enroll] enrolled against ${env.controllerUrl}`);
    return true;
  } catch (err) {
    console.warn("[ui-test-enroll] enrollment failed:", err);
    return false;
  }
}
