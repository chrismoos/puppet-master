import type { PlanDecisionMode } from "../api/plans";

export function nextPlanSelection(
  mode: PlanDecisionMode,
  current: readonly string[],
  key: string,
): string[] {
  if (mode === "dialogue") return [];
  if (mode === "single") return [key];
  return current.includes(key)
    ? current.filter((k) => k !== key)
    : [...current, key];
}

export function customOptionLabel(text: string): string {
  const firstLine = text.split("\n").map((l) => l.trim()).find((l) => l.length > 0);
  return firstLine ?? "";
}
