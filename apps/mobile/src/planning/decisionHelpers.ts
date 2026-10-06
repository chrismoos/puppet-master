import type { PlanDecision, PlanDecisionMode, PlanOption, PlanResponseInput } from "../api/plans";
import { customOptionLabel } from "./selectionHelpers";

export interface DecisionDraftState {
  selectedKeys: string[];
  customText: string;
  customActive: boolean;
  optionNotes: Record<string, string>;
  expandedOptionKey: string | null;
}

export function initialDraft(decision: PlanDecision): DecisionDraftState {
  if (decision.response) {
    const selectedKeys = decision.response.selectedOptionKeys.filter((k) => k !== "__custom__");
    const customActive = Boolean(
      decision.response.customLabel ||
      decision.response.selectedOptionKeys.includes("__custom__") ||
      decision.response.customDetailMarkdown,
    );
    return {
      selectedKeys,
      customText: decision.response.customDetailMarkdown || decision.response.customLabel || "",
      customActive,
      optionNotes: decision.response.notes ?? {},
      expandedOptionKey: null,
    };
  }
  if (decision.draft) {
    const selectedKeys = decision.draft.selectedOptionKeys.filter((k) => k !== "__custom__");
    const customActive = Boolean(
      decision.draft.selectedOptionKeys.includes("__custom__") ||
      (decision.draft.customLabel && selectedKeys.length === 0)
    );
    return {
      selectedKeys,
      customText: decision.draft.customDetailMarkdown || decision.draft.customLabel || "",
      customActive,
      optionNotes: decision.draft.notes ?? {},
      expandedOptionKey: null,
    };
  }
  return {
    selectedKeys: [],
    customText: "",
    customActive: false,
    optionNotes: {},
    expandedOptionKey: null,
  };
}

export function buildDecisionInput(
  mode: PlanDecisionMode | string,
  selectedKeys: readonly string[],
  customActive: boolean,
  customText: string,
  notes: Record<string, string>,
): PlanResponseInput {
  const singleModeCustom = customActive && mode === "single";
  return {
    selectedOptionKeys: singleModeCustom ? [] : [...selectedKeys],
    customLabel: customActive ? customOptionLabel(customText) : "",
    customDetailMarkdown: customActive ? customText : "",
    notes,
  };
}

export function buildDecisionDraftInput(
  mode: PlanDecisionMode | string,
  selectedKeys: readonly string[],
  customActive: boolean,
  customText: string,
  notes: Record<string, string>,
): PlanResponseInput {
  const optionKeys = selectedKeys.filter((k) => k !== "__custom__");
  const selectedOptionKeys = customActive
    ? mode === "single"
      ? ["__custom__"]
      : [...optionKeys, "__custom__"]
    : [...optionKeys];
  return {
    selectedOptionKeys,
    customLabel: customText.trim() ? customOptionLabel(customText) : "",
    customDetailMarkdown: customText,
    notes,
  };
}

export function validateDecision(
  mode: PlanDecisionMode | string,
  selectedKeys: readonly string[],
  customActive: boolean,
  customText: string,
  requireSelection = true,
): string | null {
  if (mode === "dialogue") return null;
  if (selectedKeys.length === 0 && !customActive) {
    if (!requireSelection) return null;
    return "Select an option before submitting.";
  }
  if (customActive && !customText.trim()) return "Enter your custom option before submitting.";
  return null;
}

export function isDecisionComplete(
  decision: PlanDecision,
  selectedKeys: readonly string[],
  customActive: boolean,
  customText: string,
): boolean {
  if (decision.mode === "dialogue") return true;
  if (customActive && !customText.trim()) return false;
  if (customActive && customText.trim()) return true;
  if (selectedKeys.length > 0) return true;
  if (decision.requireSelection === false) return true;
  return false;
}

export function reconcileSelection(
  currentKeys: readonly string[],
  newOptions: readonly PlanOption[],
): { kept: string[]; removed: string[] } {
  const available = new Set(newOptions.map((o) => o.key));
  const kept: string[] = [];
  const removed: string[] = [];
  for (const k of currentKeys) {
    if (available.has(k)) kept.push(k);
    else removed.push(k);
  }
  return { kept, removed };
}
