// Push notification registration: obtains the native APNs device token
// and registers it with the controller. Uses getDevicePushTokenAsync()
// for the raw APNs token, NOT getExpoPushTokenAsync() which returns an
// Expo relay token incompatible with direct APNs delivery.

import { NativeModules, Platform } from "react-native";
import { requireNativeModule } from "expo-modules-core";
import * as Device from "expo-device";
import * as Notifications from "expo-notifications";

import { registerPushEndpoint, type JsonFetchLike } from "./auth/api";
import type { DeviceAuthSession } from "./auth/session";

/** Prefix on every push line so a device log can be filtered to this
 * flow. Registration has four early exits that are indistinguishable
 * from success without them, and the device token is a credential, so
 * it is described by length and never printed. */
const LOG = "[push]";

/** Presents notifications that arrive while the app is open.
 *
 * iOS hands a foreground notification to the app instead of showing it,
 * so without a handler expo-notifications drops it silently and only
 * backgrounded pushes are ever seen. The badge is left alone because
 * the app is already open, which is what a badge exists to prompt. */
export function configureForegroundNotifications(): void {
  Notifications.setNotificationHandler({
    handleNotification: async () => ({
      shouldShowBanner: true,
      shouldShowList: true,
      shouldPlaySound: true,
      shouldSetBadge: false,
    }),
  });
}

/** Derives the APNs environment from the signed `aps-environment`
 * entitlement, which is what decides whether APNs accepts the device
 * token. Sending the wrong value fails as `BadDeviceToken`, which is
 * indistinguishable from a genuinely dead token.
 *
 * The build configuration cannot stand in for the entitlement: a
 * Release build installed onto a device over USB is signed with a
 * development profile, so it mints a sandbox token while `__DEV__`
 * is false.
 *
 * - Simulator: always sandbox. Since Xcode 14 simulators on Apple
 *   Silicon / T2 mint real tokens and receive real pushes, but always
 *   through the sandbox host, and ad-hoc signing grants no entitlement
 *   to read.
 * - An App Store build carries no provisioning profile, so a missing
 *   entitlement falls back to the build configuration, which can only
 *   be wrong for a profile this branch cannot see. */
function deriveApnsEnvironment(): "production" | "sandbox" {
  if (!Device.isDevice) return "sandbox";
  switch (loadPushCryptoModule()?.getApsEnvironment?.()) {
    case "development":
      return "sandbox";
    case "production":
      return "production";
    default:
      return __DEV__ ? "sandbox" : "production";
  }
}

function getLocale(): string {
  try {
    const locales = Platform.select({
      ios: () => {
        const settings = NativeModules.SettingsManager?.settings;
        const languages: unknown = settings?.AppleLanguages;
        if (Array.isArray(languages) && typeof languages[0] === "string") {
          return languages[0] as string;
        }
        return "";
      },
      default: () => "",
    });
    return locales?.() ?? "";
  } catch {
    return "";
  }
}

/** Loads the PushCrypto Expo native module, or null if it was not
 * linked. Logs a visible warning on the first miss so that a build
 * without the plugin is diagnosed immediately instead of silently
 * posting an incomplete registration. */
type PushCryptoModule = {
  getPublicKey(): Promise<string>;
  /** Absent on a build made before the module reported it. */
  getApsEnvironment?(): string | null;
  /** Clears the replay-rejection counter so the NSE accepts a fresh
   * server counter after re-enrollment. Absent on older builds. */
  resetPushCounter?(): void;
  /** Writes to os_log. Absent on older builds. */
  log?(message: string): void;
};

/** Native logging remains available without Metro. */
export function deviceLog(message: string): void {
  console.log(message);
  loadPushCryptoModule()?.log?.(message);
}

let pushCryptoModule: PushCryptoModule | null | undefined;
function loadPushCryptoModule(): PushCryptoModule | null {
  if (pushCryptoModule !== undefined) return pushCryptoModule;
  try {
    pushCryptoModule = requireNativeModule("PushCrypto");
  } catch {
    console.warn(
      `${LOG} PushCrypto native module is missing — ` +
        "push notifications will be registered without an HPKE public key " +
        "and the controller will refuse them. Rebuild with the push-crypto " +
        "config plugin applied.",
    );
    pushCryptoModule = null;
  }
  return pushCryptoModule ?? null;
}

/** Returns the device's X25519 public key (base64) for HPKE-sealed
 * push notifications. The PushCrypto native module generates and
 * persists the keypair in the shared Keychain on first call. Returns
 * undefined if the native module is unavailable (non-iOS, dev builds
 * without the plugin). */
async function getPushPublicKey(): Promise<string | undefined> {
  try {
    const PushCrypto = loadPushCryptoModule();
    if (!PushCrypto) return undefined;
    const key: unknown = await PushCrypto.getPublicKey();
    return typeof key === "string" && key.length > 0 ? key : undefined;
  } catch {
    return undefined;
  }
}

/** The APNs token this process has registered with the controller, or
 * is registering right now. `listenForTokenChanges` fires once with the
 * current token as soon as it subscribes, which lands on the token the
 * launch registration is already posting, so without this the app
 * registers twice on every launch. It is cleared on failure so a later
 * attempt is not suppressed by a registration that never landed. */
let registeredToken: string | undefined;

/** Drops the record of `token` so a failed registration can be retried. */
function forgetToken(token: string): void {
  if (registeredToken === token) registeredToken = undefined;
}

/** Asks for notification permission, obtains the native APNs token,
 * and registers it with the controller. Returns true if registration
 * succeeded, false if permission was denied or the token could not be
 * obtained. Never throws — failures are logged and silently skipped
 * so the app remains usable without push. */
export async function requestAndRegisterPush(
  auth: DeviceAuthSession,
  baseUrl: string,
  fetchImpl: JsonFetchLike,
  previewsEnabled = false,
): Promise<boolean> {
  if (Platform.OS !== "ios") {
    console.log(`${LOG} skipped, iOS only (platform ${Platform.OS})`);
    return false;
  }

  const serverDeviceId = auth.serverDeviceId();
  if (!serverDeviceId) {
    console.warn(
      `${LOG} skipped, this device is not enrolled with the controller`,
    );
    return false;
  }

  // Check current permission status before prompting.
  const { status: existingStatus } = await Notifications.getPermissionsAsync();
  let finalStatus = existingStatus;

  if (existingStatus !== "granted") {
    console.log(`${LOG} asking for permission, currently ${existingStatus}`);
    const { status } = await Notifications.requestPermissionsAsync({
      ios: { allowAlert: true, allowBadge: true, allowSound: true },
    });
    finalStatus = status;
  }

  if (finalStatus !== "granted") {
    console.warn(`${LOG} not registering, permission is ${finalStatus}`);
    return false;
  }

  let claimed: string | undefined;
  try {
    // CRITICAL: getDevicePushTokenAsync() returns the raw APNs device
    // token. Do NOT use getExpoPushTokenAsync() which returns an Expo
    // relay token that this controller cannot deliver to.
    const devicePushToken = await Notifications.getDevicePushTokenAsync();
    const token = devicePushToken.data;
    if (typeof token !== "string" || !token) {
      console.warn(`${LOG} APNs gave no token back, got ${typeof token}`);
      return false;
    }

    // The launch registration always posts, since a previews change
    // re-runs it with the same token and has to reach the controller.
    // Recording it still silences the listener's opening event.
    registeredToken = token;
    claimed = token;

    const environment = deriveApnsEnvironment();
    const locale = getLocale();
    const publicKey = await getPushPublicKey();
    console.log(
      `${LOG} got an APNs token of ${token.length} chars for ${environment}, ` +
        `registering device ${serverDeviceId}` +
        (publicKey ? " with HPKE public key" : " without HPKE key"),
    );

    // A re-enrollment gets a fresh server-side counter, so the device
    // must drop its stored counter or the NSE rejects every sealed
    // notification as a replay.
    try { loadPushCryptoModule()?.resetPushCounter?.(); } catch { /* older build */ }

    await auth.withAccessToken(baseUrl, async (accessToken) => {
      await registerPushEndpoint(baseUrl, accessToken, serverDeviceId, {
        token,
        environment,
        locale,
        previewsEnabled,
        publicKey,
      }, fetchImpl);
    });

    console.log(`${LOG} registered with the controller as ${environment}`);
    return true;
  } catch (err) {
    if (claimed) forgetToken(claimed);
    console.warn(`${LOG} registration failed`, err);
    return false;
  }
}

/** Listens for token rotation events and re-registers with the
 * controller. APNs may issue a new token at any time; this listener
 * ensures the controller always has the current one.
 *
 * The subscription's opening event carries the token the launch
 * registration is already posting, so an unchanged token is skipped and
 * only a genuine rotation reaches the controller. */
export function listenForTokenChanges(
  auth: DeviceAuthSession,
  baseUrl: string,
  fetchImpl: JsonFetchLike,
  previewsEnabled = false,
): () => void {
  const subscription = Notifications.addPushTokenListener((event) => {
    const serverDeviceId = auth.serverDeviceId();
    if (!serverDeviceId || auth.status().kind !== "enrolled") return;

    const token = event.data;
    if (typeof token !== "string" || !token) return;
    if (token === registeredToken) {
      console.log(`${LOG} token unchanged, already registered`);
      return;
    }
    registeredToken = token;

    const environment = deriveApnsEnvironment();
    const locale = getLocale();

    void (async () => {
      const publicKey = await getPushPublicKey();
      await auth.withAccessToken(baseUrl, async (accessToken) => {
        await registerPushEndpoint(baseUrl, accessToken, serverDeviceId, {
          token,
          environment,
          locale,
          previewsEnabled,
          publicKey,
        }, fetchImpl);
      });
    })().catch((err) => {
      forgetToken(token);
      console.warn(`${LOG} re-registration after token rotation failed`, err);
    });
  });

  return () => subscription.remove();
}
