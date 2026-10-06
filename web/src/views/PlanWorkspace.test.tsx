import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import type { PlanDecision } from "../api/plans";
import {
  DecisionPanel,
  customOptionLabel,
  initialDraft,
  isDecisionComplete,
  nextPlanSelection,
  toggleCustomOption,
  updateCustomTextDraft,
} from "./PlanWorkspace";

describe("planning decision selection", () => {
  it("replaces a single selection and toggles multiple selections", () => {
    expect(nextPlanSelection("single", ["one"], "two")).toEqual(["two"]);
    expect(nextPlanSelection("multiple", ["one"], "two")).toEqual(["one", "two"]);
    expect(nextPlanSelection("multiple", ["one", "two"], "one")).toEqual(["two"]);
  });

  it("turns the first line of a custom response into its option label", () => {
    expect(customOptionLabel("  Hybrid store\nUse both systems.  ")).toBe("Hybrid store");
  });

  it("requires every selectable decision to have a complete draft", () => {
    const decision = { mode: "single" } as PlanDecision;
    expect(isDecisionComplete(decision, {
      selectedOptionKeys: ["recommended"],
      customLabel: "",
      customDetailMarkdown: "",
      notes: {},
    })).toBe(true);
    expect(isDecisionComplete(decision, {
      selectedOptionKeys: ["__custom__"],
      customLabel: "My option",
      customDetailMarkdown: "My option\n\nDetails",
      notes: {},
    })).toBe(true);
    expect(isDecisionComplete(decision, {
      selectedOptionKeys: [],
      customLabel: "",
      customDetailMarkdown: "",
      notes: {},
    })).toBe(false);
  });
});

describe("custom option draft updates", () => {
  it("selects custom option and sets label when user types text in single mode", () => {
    const draft = updateCustomTextDraft("single", ["opt1"], "My custom approach\nMore details here");
    expect(draft.selectedOptionKeys).toEqual(["__custom__"]);
    expect(draft.customLabel).toBe("My custom approach");
    expect(draft.customDetailMarkdown).toBe("My custom approach\nMore details here");
    expect(draft.customOpen).toBe(true);
  });

  it("adds custom option without clearing existing selections in multiple mode", () => {
    const draft = updateCustomTextDraft("multiple", ["opt1"], "Additional custom option");
    expect(draft.selectedOptionKeys).toEqual(["opt1", "__custom__"]);
    expect(draft.customLabel).toBe("Additional custom option");
  });

  it("deselects custom option when all text is cleared", () => {
    const draft = updateCustomTextDraft("single", ["__custom__"], "   ");
    expect(draft.selectedOptionKeys).toEqual([]);
    expect(draft.customLabel).toBe("");
  });

  it("opens editor without selecting when custom head is clicked with no text", () => {
    const result = toggleCustomOption("single", ["opt1"], false);
    expect(result.selectedOptionKeys).toEqual(["opt1"]);
    expect(result.customOpen).toBe(true);
  });

  it("selects custom option when custom head is clicked with existing text", () => {
    const result = toggleCustomOption("single", ["opt1"], true);
    expect(result.selectedOptionKeys).toEqual(["__custom__"]);
    expect(result.customOpen).toBe(true);
  });
});

describe("DecisionPanel custom option rendering", () => {
  const sampleDecision: PlanDecision = {
    id: 1,
    key: "arch",
    title: "Select architecture",
    promptMarkdown: "Choose your preferred setup",
    detailMarkdown: "Architecture decision details",
    mode: "single",
    state: "open",
    allowCustom: true,
    requireSelection: true,
    batchKey: null,
    batchPosition: null,
    resolutionMarkdown: "",
    options: [
      { id: 1, key: "opt1", label: "Option One", detailMarkdown: "Option 1 info" },
      { id: 2, key: "opt2", label: "Option Two", detailMarkdown: "Option 2 info" },
    ],
    response: null,
    draft: null,
    createdAtUnixMs: 0,
    updatedAtUnixMs: 0,
  };

  it("renders Add your own option with radio indicator and no add-and-select buttons", () => {
    const html = renderToStaticMarkup(
      <DecisionPanel
        decision={sampleDecision}
        waiting={false}
        selected={["opt1"]}
        highlighted={"opt1"}
        highlightedDetail={"Option 1 info"}
        customOpen={false}
        customText={""}
        notes={{}}
        busy={false}
        batchSize={1}
        batch={[]}
        batchIndex={0}
        batchComplete={true}
        onChoose={() => {}}
        onHighlight={() => {}}
        onCustomHeadClick={() => {}}
        onCustomText={() => {}}
        onNote={() => {}}
        onPrevious={() => {}}
        onNext={() => {}}
        onJump={() => {}}
        onSubmit={() => {}}
      />,
    );

    expect(html).toContain("Add your own option");
    expect(html).toContain("plan-radio");
    expect(html).not.toContain("add &amp; select");
    expect(html).not.toContain("add & select");
    expect(html).not.toContain("plan-custom-textarea");
  });

  it("renders textarea when custom option is open", () => {
    const html = renderToStaticMarkup(
      <DecisionPanel
        decision={sampleDecision}
        waiting={false}
        selected={["opt1"]}
        highlighted={"__custom__"}
        highlightedDetail={"Option 1 info"}
        customOpen={true}
        customText={""}
        notes={{}}
        busy={false}
        batchSize={1}
        batch={[]}
        batchIndex={0}
        batchComplete={true}
        onChoose={() => {}}
        onHighlight={() => {}}
        onCustomHeadClick={() => {}}
        onCustomText={() => {}}
        onNote={() => {}}
        onPrevious={() => {}}
        onNext={() => {}}
        onJump={() => {}}
        onSubmit={() => {}}
      />,
    );

    expect(html).toContain("plan-custom-textarea");
    expect(html).toContain('placeholder="Describe your option…"');
  });

  it("shows selection mark on custom option when __custom__ is selected", () => {
    const html = renderToStaticMarkup(
      <DecisionPanel
        decision={sampleDecision}
        waiting={false}
        selected={["__custom__"]}
        highlighted={"__custom__"}
        highlightedDetail={"My custom text"}
        customOpen={true}
        customText={"My custom text"}
        notes={{}}
        busy={false}
        batchSize={1}
        batch={[]}
        batchIndex={0}
        batchComplete={true}
        onChoose={() => {}}
        onHighlight={() => {}}
        onCustomHeadClick={() => {}}
        onCustomText={() => {}}
        onNote={() => {}}
        onPrevious={() => {}}
        onNext={() => {}}
        onJump={() => {}}
        onSubmit={() => {}}
      />,
    );

    expect(html).toContain("plan-custom-option is-highlighted is-selected");
    expect(html).toContain('aria-checked="true"');
    expect(html).toContain("My custom text");
  });

  it("does not render custom option when allowCustom is false", () => {
    const html = renderToStaticMarkup(
      <DecisionPanel
        decision={{ ...sampleDecision, allowCustom: false }}
        waiting={false}
        selected={["opt1"]}
        highlighted={"opt1"}
        highlightedDetail={"Option 1 info"}
        customOpen={false}
        customText={""}
        notes={{}}
        busy={false}
        batchSize={1}
        batch={[]}
        batchIndex={0}
        batchComplete={true}
        onChoose={() => {}}
        onHighlight={() => {}}
        onCustomHeadClick={() => {}}
        onCustomText={() => {}}
        onNote={() => {}}
        onPrevious={() => {}}
        onNext={() => {}}
        onJump={() => {}}
        onSubmit={() => {}}
      />,
    );

    expect(html).not.toContain("Add your own option");
    expect(html).not.toContain("plan-custom-option");
  });
});

describe("initialDraft state restoration", () => {
  const baseDecision: PlanDecision = {
    id: 10,
    key: "db",
    title: "Database choice",
    promptMarkdown: "Select DB",
    detailMarkdown: "DB details",
    mode: "single",
    state: "open",
    allowCustom: true,
    requireSelection: true,
    batchKey: null,
    batchPosition: null,
    resolutionMarkdown: "",
    options: [
      { id: 1, key: "pg", label: "Postgres", detailMarkdown: "" },
      { id: 2, key: "sqlite", label: "SQLite", detailMarkdown: "" },
    ],
    response: null,
    draft: null,
    createdAtUnixMs: 0,
    updatedAtUnixMs: 0,
  };

  it("initializes empty draft when neither response nor server draft exists", () => {
    const draft = initialDraft(baseDecision);
    expect(draft.selectedOptionKeys).toEqual([]);
    expect(draft.highlighted).toBe("pg");
    expect(draft.customLabel).toBe("");
    expect(draft.customDetailMarkdown).toBe("");
    expect(draft.customOpen).toBe(false);
    expect(draft.notes).toEqual({});
  });

  it("restores server-saved draft selections, custom text, and notes", () => {
    const withDraft: PlanDecision = {
      ...baseDecision,
      draft: {
        selectedOptionKeys: ["sqlite"],
        customLabel: "Custom replication setup",
        customDetailMarkdown: "Custom replication setup\nUsing Litestream.",
        notes: { sqlite: "Fast and easy local backup" },
        updatedAtUnixMs: 123456,
      },
    };
    const draft = initialDraft(withDraft);
    expect(draft.selectedOptionKeys).toEqual(["sqlite"]);
    expect(draft.highlighted).toBe("sqlite");
    expect(draft.customLabel).toBe("Custom replication setup");
    expect(draft.customDetailMarkdown).toBe("Custom replication setup\nUsing Litestream.");
    expect(draft.customOpen).toBe(true);
    expect(draft.notes).toEqual({ sqlite: "Fast and easy local backup" });
  });

  it("restores server-saved draft with custom option selected", () => {
    const withCustomDraft: PlanDecision = {
      ...baseDecision,
      draft: {
        selectedOptionKeys: ["__custom__"],
        customLabel: "DuckDB",
        customDetailMarkdown: "DuckDB\nFor analytics queries.",
        notes: {},
        updatedAtUnixMs: 123456,
      },
    };
    const draft = initialDraft(withCustomDraft);
    expect(draft.selectedOptionKeys).toEqual(["__custom__"]);
    expect(draft.highlighted).toBe("__custom__");
    expect(draft.customLabel).toBe("DuckDB");
    expect(draft.customDetailMarkdown).toBe("DuckDB\nFor analytics queries.");
    expect(draft.customOpen).toBe(true);
  });

  it("gives precedence to submitted response over draft", () => {
    const withBoth: PlanDecision = {
      ...baseDecision,
      state: "waiting",
      response: {
        selectedOptionKeys: ["pg"],
        customLabel: "",
        customDetailMarkdown: "",
        notes: { pg: "Final answer" },
        submittedAtUnixMs: 200000,
      },
      draft: {
        selectedOptionKeys: ["sqlite"],
        customLabel: "Old draft",
        customDetailMarkdown: "Old draft text",
        notes: {},
        updatedAtUnixMs: 100000,
      },
    };
    const draft = initialDraft(withBoth);
    expect(draft.selectedOptionKeys).toEqual(["pg"]);
    expect(draft.notes).toEqual({ pg: "Final answer" });
    expect(draft.customLabel).toBe("");
  });
});

describe("DecisionPanel recommended badge", () => {
  const baseDecision: PlanDecision = {
    id: 10,
    key: "db",
    title: "Database Choice",
    promptMarkdown: "Select database",
    detailMarkdown: "",
    mode: "single",
    state: "open",
    allowCustom: false,
    requireSelection: true,
    batchKey: null,
    batchPosition: null,
    resolutionMarkdown: "",
    options: [
      { id: 1, key: "pg", label: "PostgreSQL", detailMarkdown: "", recommended: true },
      { id: 2, key: "my", label: "MySQL", detailMarkdown: "", recommended: false },
    ],
    response: null,
    draft: null,
    createdAtUnixMs: 1,
    updatedAtUnixMs: 1,
  };

  it("renders Recommended badge for recommended option in single mode", () => {
    const html = renderToStaticMarkup(
      <DecisionPanel
        decision={baseDecision}
        waiting={false}
        selected={["pg"]}
        highlighted={"pg"}
        highlightedDetail={""}
        customOpen={false}
        customText={""}
        notes={{}}
        busy={false}
        batchSize={1}
        batch={[]}
        batchIndex={0}
        batchComplete={true}
        onChoose={() => {}}
        onHighlight={() => {}}
        onCustomHeadClick={() => {}}
        onCustomText={() => {}}
        onNote={() => {}}
        onPrevious={() => {}}
        onNext={() => {}}
        onJump={() => {}}
        onSubmit={() => {}}
      />,
    );
    expect(html).toContain("plan-recommended-badge");
    expect(html).toContain("Recommended");
  });

  it("does not render Recommended badge when decision mode is multiple", () => {
    const multipleDecision: PlanDecision = {
      ...baseDecision,
      mode: "multiple",
    };
    const html = renderToStaticMarkup(
      <DecisionPanel
        decision={multipleDecision}
        waiting={false}
        selected={["pg"]}
        highlighted={"pg"}
        highlightedDetail={""}
        customOpen={false}
        customText={""}
        notes={{}}
        busy={false}
        batchSize={1}
        batch={[]}
        batchIndex={0}
        batchComplete={true}
        onChoose={() => {}}
        onHighlight={() => {}}
        onCustomHeadClick={() => {}}
        onCustomText={() => {}}
        onNote={() => {}}
        onPrevious={() => {}}
        onNext={() => {}}
        onJump={() => {}}
        onSubmit={() => {}}
      />,
    );
    expect(html).not.toContain("plan-recommended-badge");
  });
});

describe("DecisionPanel batch navigation", () => {
  const decision: PlanDecision = {
    id: 2,
    key: "second",
    title: "Second question",
    promptMarkdown: "",
    detailMarkdown: "",
    mode: "single",
    state: "open",
    allowCustom: false,
    requireSelection: true,
    batchKey: "batch",
    batchPosition: 1,
    resolutionMarkdown: "",
    options: [{ id: 1, key: "a", label: "A", detailMarkdown: "" }],
    response: null,
    draft: null,
    createdAtUnixMs: 0,
    updatedAtUnixMs: 0,
  };
  const batch = [
    { id: 1, title: "First question", complete: true },
    { id: 2, title: "Second question", complete: false },
    { id: 3, title: "Third question", complete: false },
  ];

  function render(batchIndex: number, waiting = false) {
    return renderToStaticMarkup(
      <DecisionPanel
        decision={decision}
        waiting={waiting}
        selected={[]}
        highlighted={null}
        highlightedDetail={""}
        customOpen={false}
        customText={""}
        notes={{}}
        busy={false}
        batch={batch}
        batchSize={batch.length}
        batchIndex={batchIndex}
        batchComplete={false}
        onChoose={() => {}}
        onHighlight={() => {}}
        onCustomHeadClick={() => {}}
        onCustomText={() => {}}
        onNote={() => {}}
        onPrevious={() => {}}
        onNext={() => {}}
        onJump={() => {}}
        onSubmit={() => {}}
      />,
    );
  }

  it("renders one step per decision, marking the current and answered ones", () => {
    const html = render(1);
    expect(html).toContain("Decision 2 of 3");
    expect(html.match(/class="plan-batch-step[ "]/g)).toHaveLength(3);
    expect(html).toContain('class="plan-batch-step is-current" aria-current="step" aria-label="Decision 2: Second question"');
    expect(html).toContain('class="plan-batch-step is-complete" aria-label="Decision 1: First question (answered)"');
    expect(html).toContain('aria-label="Decision 3: Third question"');
  });

  it("keeps Back and Next usable while the batch waits on the agent", () => {
    const html = render(1, true);
    expect(html).toContain(">Back</button>");
    expect(html).not.toContain("disabled=\"\">Back</button>");
    expect(html).toContain(">Next</button>");
  });

  it("hides the step list for a single decision", () => {
    const html = renderToStaticMarkup(
      <DecisionPanel
        decision={decision}
        waiting={false}
        selected={[]}
        highlighted={null}
        highlightedDetail={""}
        customOpen={false}
        customText={""}
        notes={{}}
        busy={false}
        batch={[batch[1]]}
        batchSize={1}
        batchIndex={0}
        batchComplete={false}
        onChoose={() => {}}
        onHighlight={() => {}}
        onCustomHeadClick={() => {}}
        onCustomText={() => {}}
        onNote={() => {}}
        onPrevious={() => {}}
        onNext={() => {}}
        onJump={() => {}}
        onSubmit={() => {}}
      />,
    );
    expect(html).not.toContain("plan-batch-steps");
  });
});
