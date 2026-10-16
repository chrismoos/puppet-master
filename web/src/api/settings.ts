import { authedFetch } from "./token";

const HTTP_UNAUTHORIZED = 401;

export interface DaemonSetting {
  key: string;
  /** The effective value: the stored one, else the default. */
  value: string;
  default: string;
  /** True when a value is explicitly stored. */
  set: boolean;
  /**
   * A credential. The value never comes back, so `value` is always empty
   * and `set` is the only thing that says whether one is stored.
   */
  secret?: boolean;
  /** The values this setting accepts, when they are a known set. */
  options?: string[];
  /**
   * Where to ask for the choices, when they are not ours to name. The
   * fetch costs a round trip to a provider, so it happens when the user
   * asks for it rather than on load.
   */
  choices_url?: string | null;
  description: string;
}

/** One choice fetched from a `choices_url`. */
export interface FetchedChoice {
  id: string;
  name: string;
}

/**
 * Asks the daemon for a setting's choices. The daemon holds the
 * credential and makes the provider call, so nothing reaches the page
 * but the list.
 */
export async function fetchChoices(url: string): Promise<FetchedChoice[]> {
  const res = await authedFetch(url);
  if (!res.ok) return fail(res, "choices fetch");
  const body = (await res.json()) as { options: FetchedChoice[] };
  return body.options;
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

/** Whether a setting holds a credential, so its value is never shown. */
export function isSecretSetting(setting: DaemonSetting): boolean {
  return setting.secret === true;
}

/** The values a setting accepts, when it is a choice rather than free text. */
export function settingOptions(setting: DaemonSetting): string[] {
  return setting.options ?? [];
}

/** Where a setting's choices can be fetched, when they are fetchable. */
export function settingChoicesUrl(setting: DaemonSetting): string | null {
  return setting.choices_url ?? null;
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
  "Voice",
  "Mobile app",
] as const;

export const CATEGORY_DESCRIPTIONS: Record<string, string> = {
  "Agent terminal": "How spawned agents see their terminal",
  "Supervisors": "Session concurrency limits",
  "Voice": "What the microphone reasons with and speaks through",
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
  "voice.model.profile": {
    title: "Model profile the voice agent reasons with",
    category: "Voice",
  },
  "voice.tts.provider": { title: "Speech provider", category: "Voice" },
  "voice.tts.voice": { title: "Voice", category: "Voice" },
  "voice.tts.base_url": { title: "Speech provider base URL", category: "Voice" },
  "voice.tts.api_key": { title: "Speech provider API key", category: "Voice" },
  "voice.tts.format": { title: "Audio format", category: "Voice" },
  "voice.tts.model": { title: "Speech model", category: "Voice" },
};

export function settingMetadata(key: string): SettingMeta {
  const known = KNOWN_SETTING_METAS[key];
  let category = known?.category;
  if (!category) {
    if (key.startsWith("spawn.")) category = "Agent terminal";
    else if (key.startsWith("supervisor.")) category = "Supervisors";
    else if (key.startsWith("mobile.") || key.startsWith("push.")) category = "Mobile app";
    else if (key.startsWith("voice.")) category = "Voice";
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
