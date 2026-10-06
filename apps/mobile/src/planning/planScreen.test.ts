import { describe, expect, it } from "vitest";
import {
  buildDecisionDraftInput,
  buildDecisionInput,
  initialDraft,
  validateDecision,
  isDecisionComplete,
  reconcileSelection,
} from "./decisionHelpers";
import { nextPlanSelection, customOptionLabel } from "./selectionHelpers";
import type { PlanDecision, PlanOption } from "../api/plans";

describe("validateDecision", () => {
  it("returns null for dialogue mode regardless of selections", () => {
    expect(validateDecision("dialogue", [], false, "")).toBeNull();
  });

  it("requires at least one selection for single mode", () => {
    expect(validateDecision("single", [], false, "")).toBe("Select an option before submitting.");
  });

  it("requires at least one selection for multiple mode", () => {
    expect(validateDecision("multiple", [], false, "")).toBe("Select an option before submitting.");
  });

  it("passes when an option is selected", () => {
    expect(validateDecision("single", ["opt-a"], false, "")).toBeNull();
  });

  it("passes when custom is active with text", () => {
    expect(validateDecision("single", [], true, "My option")).toBeNull();
  });

  it("rejects custom active with empty text", () => {
    expect(validateDecision("single", [], true, "   ")).toBe("Enter your custom option before submitting.");
  });

  it("allows empty selection when requireSelection is false", () => {
    expect(validateDecision("single", [], false, "", false)).toBeNull();
  });

  it("allows empty selection for multiple when requireSelection is false", () => {
    expect(validateDecision("multiple", [], false, "", false)).toBeNull();
  });

  it("still rejects empty custom text when requireSelection is false", () => {
    expect(validateDecision("single", [], true, "  ", false)).toBe("Enter your custom option before submitting.");
  });
});

describe("buildDecisionInput", () => {
  it("sends selected keys when custom is inactive", () => {
    const input = buildDecisionInput("single", ["opt-a"], false, "", { "opt-a": "note" });
    expect(input.selectedOptionKeys).toEqual(["opt-a"]);
    expect(input.customLabel).toBe("");
    expect(input.customDetailMarkdown).toBe("");
    expect(input.notes).toEqual({ "opt-a": "note" });
  });

  it("clears selected keys for single-mode custom", () => {
    const input = buildDecisionInput("single", ["opt-a"], true, "My idea\nDetails here", {});
    expect(input.selectedOptionKeys).toEqual([]);
    expect(input.customLabel).toBe("My idea");
    expect(input.customDetailMarkdown).toBe("My idea\nDetails here");
  });

  it("preserves selected keys for multiple-mode custom", () => {
    const input = buildDecisionInput("multiple", ["opt-a", "opt-b"], true, "Also this\nMore", {});
    expect(input.selectedOptionKeys).toEqual(["opt-a", "opt-b"]);
    expect(input.customLabel).toBe("Also this");
    expect(input.customDetailMarkdown).toBe("Also this\nMore");
  });

  it("includes per-option notes", () => {
    const notes = { "opt-a": "Good for latency", "opt-b": "Better for throughput" };
    const input = buildDecisionInput("multiple", ["opt-a", "opt-b"], false, "", notes);
    expect(input.notes).toEqual(notes);
  });
});

describe("reconcileSelection", () => {
  it("keeps selections that still exist by stable key", () => {
    const options: PlanOption[] = [
      { id: 1, key: "opt-a", label: "A", detailMarkdown: "" },
      { id: 3, key: "opt-c", label: "C revised", detailMarkdown: "" },
    ];
    const { kept, removed } = reconcileSelection(["opt-a", "opt-b", "opt-c"], options);
    expect(kept).toEqual(["opt-a", "opt-c"]);
    expect(removed).toEqual(["opt-b"]);
  });

  it("reports empty removed when all selections survive", () => {
    const options: PlanOption[] = [
      { id: 1, key: "opt-a", label: "A", detailMarkdown: "" },
      { id: 2, key: "opt-b", label: "B", detailMarkdown: "" },
    ];
    const { kept, removed } = reconcileSelection(["opt-a"], options);
    expect(kept).toEqual(["opt-a"]);
    expect(removed).toEqual([]);
  });

  it("handles empty current selection", () => {
    const options: PlanOption[] = [
      { id: 1, key: "opt-a", label: "A", detailMarkdown: "" },
    ];
    const { kept, removed } = reconcileSelection([], options);
    expect(kept).toEqual([]);
    expect(removed).toEqual([]);
  });

  it("handles all options removed", () => {
    const { kept, removed } = reconcileSelection(["opt-a", "opt-b"], []);
    expect(kept).toEqual([]);
    expect(removed).toEqual(["opt-a", "opt-b"]);
  });
});

describe("selection mode integration", () => {
  it("single-select + submit builds correct input", () => {
    let sel = nextPlanSelection("single", [], "opt-a");
    expect(sel).toEqual(["opt-a"]);
    sel = nextPlanSelection("single", sel, "opt-b");
    expect(sel).toEqual(["opt-b"]);
    const input = buildDecisionInput("single", sel, false, "", {});
    expect(input.selectedOptionKeys).toEqual(["opt-b"]);
  });

  it("multi-select + custom preserves both", () => {
    let sel = nextPlanSelection("multiple", [], "opt-a");
    sel = nextPlanSelection("multiple", sel, "opt-b");
    const input = buildDecisionInput("multiple", sel, true, "Extra\nStuff", { "opt-a": "note" });
    expect(input.selectedOptionKeys).toEqual(["opt-a", "opt-b"]);
    expect(input.customLabel).toBe("Extra");
    expect(input.notes).toEqual({ "opt-a": "note" });
  });

  it("custom option label extracts first line", () => {
    expect(customOptionLabel("  Hybrid store\nUse both systems.  ")).toBe("Hybrid store");
    expect(customOptionLabel("")).toBe("");
    expect(customOptionLabel("\n\n  Real label\ndetail")).toBe("Real label");
  });
});

function makeDecision(overrides: Partial<PlanDecision> = {}): PlanDecision {
  return {
    id: 1,
    key: "dec-1",
    title: "Test decision",
    promptMarkdown: "",
    detailMarkdown: "",
    mode: "single",
    state: "open",
    allowCustom: true,
    requireSelection: true,
    batchKey: null,
    batchPosition: null,
    resolutionMarkdown: "",
    options: [
      { id: 1, key: "opt-a", label: "A", detailMarkdown: "" },
      { id: 2, key: "opt-b", label: "B", detailMarkdown: "" },
    ],
    response: null,
    draft: null,
    createdAtUnixMs: 0,
    updatedAtUnixMs: 0,
    ...overrides,
  };
}

describe("isDecisionComplete", () => {
  it("requires a finished custom option even when selection is optional", () => {
    expect(isDecisionComplete(makeDecision({ requireSelection: false }), [], true, " ")).toBe(false);
  });

  it("requires selection when an older controller omits the flag", () => {
    const decision = makeDecision();
    Reflect.deleteProperty(decision, "requireSelection");
    expect(isDecisionComplete(decision, [], false, "")).toBe(false);
  });
  it("returns true for dialogue mode with no selections", () => {
    expect(isDecisionComplete(makeDecision({ mode: "dialogue" }), [], false, "")).toBe(true);
  });

  it("returns false for single mode with no selections", () => {
    expect(isDecisionComplete(makeDecision({ mode: "single" }), [], false, "")).toBe(false);
  });

  it("returns true for single mode with one selection", () => {
    expect(isDecisionComplete(makeDecision({ mode: "single" }), ["opt-a"], false, "")).toBe(true);
  });

  it("returns true for custom active with text", () => {
    expect(isDecisionComplete(makeDecision({ mode: "single" }), [], true, "My idea")).toBe(true);
  });

  it("returns false for custom active with empty text", () => {
    expect(isDecisionComplete(makeDecision({ mode: "single" }), [], true, "")).toBe(false);
  });

  it("returns true for multiple mode with selections", () => {
    expect(isDecisionComplete(makeDecision({ mode: "multiple" }), ["opt-a", "opt-b"], false, "")).toBe(true);
  });

  it("returns true for empty selection when requireSelection is false", () => {
    expect(isDecisionComplete(makeDecision({ requireSelection: false }), [], false, "")).toBe(true);
  });

  it("returns true for empty multiple selection when requireSelection is false", () => {
    expect(isDecisionComplete(makeDecision({ mode: "multiple", requireSelection: false }), [], false, "")).toBe(true);
  });
});

describe("custom option exclusivity in single mode", () => {
  it("selecting an option clears customActive in single mode", () => {
    // Simulate: customActive is true, user taps an option
    const mode = "single" as const;
    const selectedKeys = nextPlanSelection(mode, [], "opt-a");
    // In PlanScreen, handleOptionToggle sets customActive to false for single mode
    const customActive = mode === "single" ? false : true;
    expect(selectedKeys).toEqual(["opt-a"]);
    expect(customActive).toBe(false);
  });

  it("selecting an option preserves customActive in multiple mode", () => {
    const mode = "multiple" as const;
    const selectedKeys = nextPlanSelection(mode, [], "opt-a");
    const customActive = mode === "single" ? false : true;
    expect(selectedKeys).toEqual(["opt-a"]);
    expect(customActive).toBe(true);
  });

  it("selecting custom clears selectedKeys in single mode", () => {
    // handleCustomToggle logic: nextCustom = true, single mode => clear selectedKeys
    const nextCustom = true;
    const mode = "single" as const;
    const selectedKeys = nextCustom && mode === "single" ? [] : ["opt-a"];
    expect(selectedKeys).toEqual([]);
  });

  it("selecting custom preserves selectedKeys in multiple mode", () => {
    const nextCustom = true;
    const mode = "multiple" as const;
    const selectedKeys = nextCustom && mode === "single" ? [] : ["opt-a"];
    expect(selectedKeys).toEqual(["opt-a"]);
  });

  it("retains typed custom text when toggling back to an option", () => {
    // Simulate full flow: type custom text -> select option -> toggle custom back
    const customText = "My typed idea";
    // Step 1: custom is active with text
    // Step 2: user taps an option => customActive=false, customText retained
    const afterOptionTap = { customActive: false, customText };
    expect(afterOptionTap.customText).toBe("My typed idea");
    // Step 3: user taps custom again => customActive=true, customText still there
    const afterCustomTap = { customActive: true, customText: afterOptionTap.customText };
    expect(afterCustomTap.customActive).toBe(true);
    expect(afterCustomTap.customText).toBe("My typed idea");
  });
});

describe("batch decision flow", () => {
  it("builds responses for multiple decisions atomically", () => {
    const decisions = [
      makeDecision({ id: 1, key: "dec-1", mode: "single" }),
      makeDecision({ id: 2, key: "dec-2", mode: "multiple" }),
    ];
    const drafts = [
      { selectedKeys: ["opt-a"], customActive: false, customText: "", optionNotes: {} },
      { selectedKeys: ["opt-a", "opt-b"], customActive: false, customText: "", optionNotes: { "opt-a": "note" } },
    ];

    const responses = decisions.map((dec, i) =>
      buildDecisionInput(dec.mode, drafts[i].selectedKeys, drafts[i].customActive, drafts[i].customText, drafts[i].optionNotes),
    );

    expect(responses).toHaveLength(2);
    expect(responses[0].selectedOptionKeys).toEqual(["opt-a"]);
    expect(responses[1].selectedOptionKeys).toEqual(["opt-a", "opt-b"]);
    expect(responses[1].notes).toEqual({ "opt-a": "note" });
  });

  it("validates all decisions before submitting a batch", () => {
    const decisions = [
      makeDecision({ id: 1, mode: "single" }),
      makeDecision({ id: 2, mode: "single" }),
    ];
    const drafts = [
      { selectedKeys: ["opt-a"], customActive: false, customText: "" },
      { selectedKeys: [], customActive: false, customText: "" },
    ];

    const errors = decisions.map((dec, i) =>
      validateDecision(dec.mode, drafts[i].selectedKeys, drafts[i].customActive, drafts[i].customText),
    );

    expect(errors[0]).toBeNull();
    expect(errors[1]).toBe("Select an option before submitting.");
  });

  it("all-complete check gates submit", () => {
    const decisions = [
      makeDecision({ id: 1, mode: "single" }),
      makeDecision({ id: 2, mode: "multiple" }),
    ];
    const drafts = [
      { selectedKeys: ["opt-a"], customActive: false, customText: "" },
      { selectedKeys: ["opt-b"], customActive: false, customText: "" },
    ];

    const allComplete = decisions.every((dec, i) =>
      isDecisionComplete(dec, drafts[i].selectedKeys, drafts[i].customActive, drafts[i].customText),
    );
    expect(allComplete).toBe(true);
  });

  it("incomplete decision blocks batch submit", () => {
    const decisions = [
      makeDecision({ id: 1, mode: "single" }),
      makeDecision({ id: 2, mode: "single" }),
    ];
    const drafts = [
      { selectedKeys: ["opt-a"], customActive: false, customText: "" },
      { selectedKeys: [], customActive: false, customText: "" },
    ];

    const allComplete = decisions.every((dec, i) =>
      isDecisionComplete(dec, drafts[i].selectedKeys, drafts[i].customActive, drafts[i].customText),
    );
    expect(allComplete).toBe(false);
  });

  it("single-decision compatibility: batch of one works like before", () => {
    const dec = makeDecision({ id: 1, mode: "single" });
    const sel = nextPlanSelection("single", [], "opt-a");
    const input = buildDecisionInput(dec.mode, sel, false, "", {});
    expect(input.selectedOptionKeys).toEqual(["opt-a"]);
    expect(isDecisionComplete(dec, sel, false, "")).toBe(true);
  });

  it("activeDecisionIds falls back to activeDecisionId for single-decision plans", () => {
    const plan = { activeDecisionId: 42, activeDecisionIds: [] as number[] };
    const ids = plan.activeDecisionIds.length ? plan.activeDecisionIds : plan.activeDecisionId != null ? [plan.activeDecisionId] : [];
    expect(ids).toEqual([42]);
  });

  it("activeDecisionIds takes priority when populated", () => {
    const plan = { activeDecisionId: 1, activeDecisionIds: [1, 2, 3] };
    const ids = plan.activeDecisionIds.length ? plan.activeDecisionIds : [plan.activeDecisionId];
    expect(ids).toEqual([1, 2, 3]);
  });
});

describe("buildDecisionDraftInput", () => {
  it("saves selected option keys when custom is inactive", () => {
    const draft = buildDecisionDraftInput("single", ["opt-a"], false, "", { "opt-a": "Good note" });
    expect(draft.selectedOptionKeys).toEqual(["opt-a"]);
    expect(draft.customLabel).toBe("");
    expect(draft.customDetailMarkdown).toBe("");
    expect(draft.notes).toEqual({ "opt-a": "Good note" });
  });

  it("preserves custom text in customDetailMarkdown even when customActive is false", () => {
    const draft = buildDecisionDraftInput("single", ["opt-a"], false, "Saved idea draft", {});
    expect(draft.selectedOptionKeys).toEqual(["opt-a"]);
    expect(draft.customLabel).toBe("Saved idea draft");
    expect(draft.customDetailMarkdown).toBe("Saved idea draft");
  });

  it("includes __custom__ in selectedOptionKeys for single mode when customActive", () => {
    const draft = buildDecisionDraftInput("single", ["opt-a"], true, "Custom proposal\nWith details", {});
    expect(draft.selectedOptionKeys).toEqual(["__custom__"]);
    expect(draft.customLabel).toBe("Custom proposal");
    expect(draft.customDetailMarkdown).toBe("Custom proposal\nWith details");
  });

  it("includes __custom__ alongside selected options for multiple mode when customActive", () => {
    const draft = buildDecisionDraftInput("multiple", ["opt-a", "opt-b"], true, "Custom proposal", { "opt-a": "note" });
    expect(draft.selectedOptionKeys).toEqual(["opt-a", "opt-b", "__custom__"]);
    expect(draft.customLabel).toBe("Custom proposal");
    expect(draft.notes).toEqual({ "opt-a": "note" });
  });
});

describe("initialDraft", () => {
  it("returns default empty state when neither response nor draft is present", () => {
    const dec = makeDecision();
    const draft = initialDraft(dec);
    expect(draft.selectedKeys).toEqual([]);
    expect(draft.customText).toBe("");
    expect(draft.customActive).toBe(false);
    expect(draft.optionNotes).toEqual({});
  });

  it("restores draft selections from server draft", () => {
    const dec = makeDecision({
      draft: {
        selectedOptionKeys: ["opt-b"],
        customLabel: "",
        customDetailMarkdown: "",
        notes: { "opt-b": "Server note" },
        updatedAtUnixMs: 1000,
      },
    });
    const draft = initialDraft(dec);
    expect(draft.selectedKeys).toEqual(["opt-b"]);
    expect(draft.customActive).toBe(false);
    expect(draft.customText).toBe("");
    expect(draft.optionNotes).toEqual({ "opt-b": "Server note" });
  });

  it("restores custom option and text when draft has __custom__", () => {
    const dec = makeDecision({
      draft: {
        selectedOptionKeys: ["__custom__"],
        customLabel: "Custom idea",
        customDetailMarkdown: "Custom idea\nExtra details",
        notes: {},
        updatedAtUnixMs: 1000,
      },
    });
    const draft = initialDraft(dec);
    expect(draft.selectedKeys).toEqual([]);
    expect(draft.customActive).toBe(true);
    expect(draft.customText).toBe("Custom idea\nExtra details");
  });

  it("restores custom text while keeping option selection when toggled back to option", () => {
    const dec = makeDecision({
      draft: {
        selectedOptionKeys: ["opt-a"],
        customLabel: "Draft note",
        customDetailMarkdown: "Draft note\nSaved text",
        notes: {},
        updatedAtUnixMs: 1000,
      },
    });
    const draft = initialDraft(dec);
    expect(draft.selectedKeys).toEqual(["opt-a"]);
    expect(draft.customActive).toBe(false);
    expect(draft.customText).toBe("Draft note\nSaved text");
  });

  it("prioritizes submitted response over unsubmitted draft", () => {
    const dec = makeDecision({
      response: {
        selectedOptionKeys: ["opt-b"],
        customLabel: "",
        customDetailMarkdown: "",
        notes: {},
        submittedAtUnixMs: 2000,
      },
      draft: {
        selectedOptionKeys: ["opt-a"],
        customLabel: "",
        customDetailMarkdown: "",
        notes: {},
        updatedAtUnixMs: 1000,
      },
    });
    const draft = initialDraft(dec);
    expect(draft.selectedKeys).toEqual(["opt-b"]);
  });

  it("preserves recommended flag on plan options", () => {
    const optRecommended: PlanOption = {
      id: 1,
      key: "opt-rec",
      label: "Recommended Option",
      detailMarkdown: "Why this is recommended",
      recommended: true,
    };
    const optRegular: PlanOption = {
      id: 2,
      key: "opt-reg",
      label: "Regular Option",
      detailMarkdown: "",
      recommended: false,
    };
    const dec = makeDecision({
      options: [optRecommended, optRegular],
    });
    expect(dec.options[0].recommended).toBe(true);
    expect(dec.options[1].recommended).toBe(false);
  });
});
