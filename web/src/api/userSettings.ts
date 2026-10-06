import { APPEARANCES, type Appearance } from "@puppet-master/client-core/theme/appearance";
import { uiThemeFromStoredValue, type UiTheme } from "@puppet-master/client-core/theme/uiTheme";
import { validateTerminalTheme, type TerminalTheme } from "@puppet-master/client-core/theme/terminalTheme";
import { authedFetch } from "./token";

const HTTP_UNAUTHORIZED = 401;

/** Synchronized user-settings key holding the push idle threshold. */
export const PUSH_WEB_IDLE_MINUTES_KEY = "push.web_idle_minutes";
export const PUSH_WEB_IDLE_MINUTES_DEFAULT = 3;
export const PUSH_WEB_IDLE_MINUTES_MAX = 1440;

export async function applyTerminalTheme(theme: TerminalTheme): Promise<TerminalTheme> {
  const response = await authedFetch("/api/user/settings/terminal-theme", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(theme),
  });
  if (response.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  const body = await response.json().catch(() => ({})) as { terminalTheme?: unknown; error?: string };
  if (!response.ok) throw new Error(body.error ?? `theme update failed (${response.status})`);
  return validateTerminalTheme(body.terminalTheme);
}

export async function resetTerminalTheme(): Promise<void> {
  const response = await authedFetch("/api/user/settings/terminal-theme", { method: "DELETE" });
  if (response.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  if (!response.ok) {
    const body = await response.json().catch(() => ({})) as { error?: string };
    throw new Error(body.error ?? `theme reset failed (${response.status})`);
  }
}

export async function applyAppearance(appearance: Appearance): Promise<Appearance> {
  const response = await authedFetch("/api/user/settings/appearance", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(appearance),
  });
  if (response.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  const body = await response.json().catch(() => ({})) as { appearance?: unknown; error?: string };
  if (!response.ok) throw new Error(body.error ?? `appearance update failed (${response.status})`);
  if (!APPEARANCES.includes(body.appearance as Appearance)) {
    throw new Error("daemon returned an unknown appearance");
  }
  return body.appearance as Appearance;
}

export async function resetAppearance(): Promise<void> {
  const response = await authedFetch("/api/user/settings/appearance", { method: "DELETE" });
  if (response.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  if (!response.ok) {
    const body = await response.json().catch(() => ({})) as { error?: string };
    throw new Error(body.error ?? `appearance reset failed (${response.status})`);
  }
}

export async function applyUiTheme(theme: UiTheme): Promise<UiTheme> {
  const response = await authedFetch("/api/user/settings/ui-theme", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(theme),
  });
  if (response.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  const body = await response.json().catch(() => ({})) as { uiTheme?: unknown; error?: string };
  if (!response.ok) throw new Error(body.error ?? `ui theme update failed (${response.status})`);
  const saved = uiThemeFromStoredValue(body.uiTheme);
  if (saved === undefined) throw new Error("daemon returned an unknown ui theme");
  return saved;
}

export async function resetUiTheme(): Promise<void> {
  const response = await authedFetch("/api/user/settings/ui-theme", { method: "DELETE" });
  if (response.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  if (!response.ok) {
    const body = await response.json().catch(() => ({})) as { error?: string };
    throw new Error(body.error ?? `ui theme reset failed (${response.status})`);
  }
}

export async function applyPushWebIdleMinutes(minutes: number): Promise<number> {
  const response = await authedFetch("/api/user/settings/push-web-idle-minutes", {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify(minutes),
  });
  if (response.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  const body = await response.json().catch(() => ({})) as { pushWebIdleMinutes?: unknown; error?: string };
  if (!response.ok) throw new Error(body.error ?? `notification setting update failed (${response.status})`);
  if (typeof body.pushWebIdleMinutes !== "number") {
    throw new Error("daemon returned no idle threshold");
  }
  return body.pushWebIdleMinutes;
}
