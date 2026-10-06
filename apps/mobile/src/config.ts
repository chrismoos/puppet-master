import type { KeyValueStorage } from "@puppet-master/client-core/platform";

export const CONFIG_KEYS = {
  baseUrl: "controller.baseUrl",
  sessionToken: "controller.sessionToken",
  pushPreviews: "push.previewsEnabled",
} as const;

export const ALL_CONFIG_KEYS: readonly string[] = Object.values(CONFIG_KEYS);

export interface ControllerConfig {
  /** Canonical http(s) origin of the controller. */
  baseUrl: string;
  /** Legacy cookie-session token from the retired login bridge; kept only
   * so an already-logged-in install stays connected until it re-enrolls. */
  sessionToken: string | null;
  /** Show project name and agent headline in push notifications. When
   * off (default), notifications show only "New notification". When on,
   * the content is visible to Apple on the direct APNs path. */
  pushPreviewsEnabled: boolean;
}

export function readControllerConfig(storage: KeyValueStorage): ControllerConfig | null {
  const baseUrl = storage.getItem(CONFIG_KEYS.baseUrl);
  if (!baseUrl) return null;
  return {
    baseUrl,
    sessionToken: storage.getItem(CONFIG_KEYS.sessionToken) || null,
    pushPreviewsEnabled: storage.getItem(CONFIG_KEYS.pushPreviews) === "true",
  };
}

export function writeControllerConfig(storage: KeyValueStorage, config: ControllerConfig): void {
  storage.setItem(CONFIG_KEYS.baseUrl, config.baseUrl);
  storage.setItem(CONFIG_KEYS.sessionToken, config.sessionToken ?? "");
  storage.setItem(CONFIG_KEYS.pushPreviews, config.pushPreviewsEnabled ? "true" : "false");
}
