import type { KeyValueStorage } from "../platform";

export const PLAN_FOCUS_KEY = "pm.plan.focusedDecision";

/** Plans whose focused decision is remembered before the oldest is dropped. */
export const PLAN_FOCUS_LIMIT = 50;

type FocusMap = Record<string, number>;

function readMap(storage: KeyValueStorage): FocusMap {
  const raw = storage.getItem(PLAN_FOCUS_KEY);
  if (!raw) return {};
  try {
    const parsed = JSON.parse(raw) as unknown;
    if (!parsed || typeof parsed !== "object" || Array.isArray(parsed)) return {};
    const map: FocusMap = {};
    for (const [planId, decisionId] of Object.entries(parsed as Record<string, unknown>)) {
      if (typeof decisionId === "number" && Number.isInteger(decisionId)) map[planId] = decisionId;
    }
    return map;
  } catch {
    return {};
  }
}

export function readFocusedDecision(storage: KeyValueStorage, planId: string): number | null {
  return readMap(storage)[planId] ?? null;
}

export function writeFocusedDecision(storage: KeyValueStorage, planId: string, decisionId: number): void {
  const map = readMap(storage);
  delete map[planId];
  const entries = Object.entries(map);
  entries.push([planId, decisionId]);
  const kept = entries.slice(Math.max(0, entries.length - PLAN_FOCUS_LIMIT));
  storage.setItem(PLAN_FOCUS_KEY, JSON.stringify(Object.fromEntries(kept)));
}

/**
 * Picks which decision of a batch to show: the one already focused when it is
 * still active, otherwise the remembered one, otherwise the first.
 */
export function resolveFocusedDecision(
  activeIds: readonly number[],
  current: number | null,
  remembered: number | null,
): number | null {
  if (current !== null && activeIds.includes(current)) return current;
  if (remembered !== null && activeIds.includes(remembered)) return remembered;
  return activeIds[0] ?? null;
}
