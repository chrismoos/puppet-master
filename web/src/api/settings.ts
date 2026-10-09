import { authedFetch } from "./token";

const HTTP_UNAUTHORIZED = 401;

export interface DaemonSetting {
  key: string;
  /** The effective value: the stored one, else the default. */
  value: string;
  default: string;
  /** True when a value is explicitly stored. */
  set: boolean;
  description: string;
}

async function fail(res: Response, what: string): Promise<never> {
  if (res.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  const body = (await res.json().catch(() => null)) as { error?: string } | null;
  throw new Error(body?.error || `${what} failed (${res.status})`);
}

export async function fetchSettings(signal?: AbortSignal): Promise<DaemonSetting[]> {
  const res = await authedFetch("/api/settings", { signal });
  if (!res.ok) return fail(res, "settings fetch");
  const body = (await res.json()) as { settings: DaemonSetting[] };
  return body.settings;
}

/** Writes one setting; null resets it to its default. */
export async function updateSetting(key: string, value: string | null): Promise<void> {
  const res = await authedFetch(`/api/settings/${encodeURIComponent(key)}`, {
    method: "PUT",
    headers: { "content-type": "application/json" },
    body: JSON.stringify({ value }),
  });
  if (!res.ok) return fail(res, "setting update");
}

/** Whether a setting renders as a checkbox rather than a text field. */
export function isBooleanSetting(setting: DaemonSetting): boolean {
  return setting.default === "true" || setting.default === "false";
}

/** Whether a setting takes an integer numeric value. */
export function isNumericSetting(setting: DaemonSetting): boolean {
  return /^\d+$/.test(setting.default);
}

export interface SettingMeta {
  title: string;
  category: string;
  categoryDescription?: string;
  unit?: string;
  step: number;
  min: number;
  max?: number;
}

export const CATEGORY_ORDER = [
  "Agent terminal",
  "Supervisors",
  "Mobile app",
] as const;

export const CATEGORY_DESCRIPTIONS: Record<string, string> = {
  "Agent terminal": "How spawned agents see their terminal",
  "Supervisors": "Session concurrency limits",
  "Mobile app": "Token lifetimes and push delivery",
};

const KNOWN_SETTING_METAS: Record<string, Partial<SettingMeta>> = {
  "spawn.fullscreen": {
    title: "Fullscreen alternate screen for Claude Code",
    category: "Agent terminal",
  },
  "spawn.truecolor": {
    title: "24-bit truecolor",
    category: "Agent terminal",
  },
  "spawn.program_status": {
    title: "Program Status (OSC 7501) reports",
    category: "Agent terminal",
  },
  "supervisor.max_children": {
    title: "Concurrent children per supervisor",
    category: "Supervisors",
    unit: "sessions",
    min: 1,
    step: 1,
  },
  "mobile.access_token_ttl_minutes": {
    title: "Access token lifetime",
    category: "Mobile app",
    unit: "minutes",
    min: 1,
    step: 5,
  },
  "mobile.refresh_token_ttl_days": {
    title: "Refresh token lifetime",
    category: "Mobile app",
    unit: "days",
    min: 1,
    step: 1,
  },
  "mobile.enrollment_token_ttl_minutes": {
    title: "Enrollment token lifetime",
    category: "Mobile app",
    unit: "minutes",
    min: 1,
    step: 5,
  },
  "mobile.socket_ticket_ttl_seconds": {
    title: "Socket ticket lifetime",
    category: "Mobile app",
    unit: "seconds",
    min: 1,
    step: 5,
  },
  "push.dedupe_window_hours": {
    title: "Push deduplication window",
    category: "Mobile app",
    unit: "hours",
    min: 1,
    step: 1,
  },
  "push.events": {
    title: "Push notification events",
    category: "Mobile app",
  },
  "push.scope": {
    title: "Push notification recipient scope",
    category: "Mobile app",
  },
  "push.gateway.url": {
    title: "Push gateway relay URL",
    category: "Mobile app",
  },
};

export function settingMetadata(key: string): SettingMeta {
  const known = KNOWN_SETTING_METAS[key];
  let category = known?.category;
  if (!category) {
    if (key.startsWith("spawn.")) category = "Agent terminal";
    else if (key.startsWith("supervisor.")) category = "Supervisors";
    else if (key.startsWith("mobile.") || key.startsWith("push.")) category = "Mobile app";
    else category = "General";
  }

  let unit = known?.unit;
  if (!unit) {
    if (key.endsWith("_minutes")) unit = "minutes";
    else if (key.endsWith("_seconds")) unit = "seconds";
    else if (key.endsWith("_hours")) unit = "hours";
    else if (key.endsWith("_days")) unit = "days";
  }

  const defaultTitle = () => {
    const tail = key.includes(".") ? key.slice(key.indexOf(".") + 1) : key;
    return tail.replace(/_/g, " ").replace(/^\w/, (c) => c.toUpperCase());
  };

  return {
    title: known?.title ?? defaultTitle(),
    category,
    categoryDescription: CATEGORY_DESCRIPTIONS[category],
    unit,
    step: known?.step ?? 1,
    min: known?.min ?? 1,
    max: known?.max,
  };
}
