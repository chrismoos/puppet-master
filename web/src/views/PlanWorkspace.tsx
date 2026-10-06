import { useEffect, useMemo, useRef, useState } from "react";
import type { KeyValueStorage } from "@puppet-master/client-core/platform";
import { readFocusedDecision, resolveFocusedDecision, writeFocusedDecision } from "@puppet-master/client-core/state/planFocus";
import { Markdown } from "../components/Markdown";
import { readString, writeString } from "../storage";
import {
  fetchPlan,
  postPlanMessage,
  savePlanDraft,
  submitPlanDecisions,
  type PlanDecision,
  type PlanDetail,
  type PlanResponseInput,
} from "../api/plans";

export function nextPlanSelection(
  mode: PlanDecision["mode"],
  current: readonly string[],
  key: string,
): string[] {
  if (mode !== "multiple") return [key];
  return current.includes(key)
    ? current.filter((entry) => entry !== key)
    : [...current, key];
}

export function customOptionLabel(markdown: string): string {
  return markdown.trim().split(/\r?\n/, 1)[0];
}

export function updateCustomTextDraft(
  mode: PlanDecision["mode"],
  currentSelected: readonly string[],
  text: string,
): {
  customLabel: string;
  customDetailMarkdown: string;
  selectedOptionKeys: string[];
  customOpen: boolean;
} {
  const label = customOptionLabel(text);
  const hasText = text.trim().length > 0;
  let selectedOptionKeys = [...currentSelected];

  if (hasText) {
    if (!selectedOptionKeys.includes("__custom__")) {
      selectedOptionKeys = nextPlanSelection(mode, currentSelected, "__custom__");
    }
  } else {
    selectedOptionKeys = selectedOptionKeys.filter((key) => key !== "__custom__");
  }

  return {
    customLabel: label,
    customDetailMarkdown: text,
    selectedOptionKeys,
    customOpen: true,
  };
}

export function toggleCustomOption(
  mode: PlanDecision["mode"],
  currentSelected: readonly string[],
  hasText: boolean,
): {
  selectedOptionKeys: string[];
  customOpen: boolean;
} {
  if (!hasText) {
    return {
      selectedOptionKeys: [...currentSelected],
      customOpen: true,
    };
  }
  return {
    selectedOptionKeys: nextPlanSelection(mode, currentSelected, "__custom__"),
    customOpen: true,
  };
}

const preferences: KeyValueStorage = { getItem: readString, setItem: writeString };

export interface BatchEntry {
  id: number;
  title: string;
  complete: boolean;
}

interface DecisionDraft extends PlanResponseInput {
  highlighted: string | null;
  customOpen: boolean;
}

export function initialDraft(decision: PlanDecision): DecisionDraft {
  if (decision.response) {
    const selectedOptionKeys = decision.response.selectedOptionKeys ?? [];
    const customDetail = decision.response.customDetailMarkdown || decision.response.customLabel || "";
    const hasCustom = Boolean(decision.response.customLabel || customDetail.trim());
    return {
      selectedOptionKeys: hasCustom ? [...selectedOptionKeys, "__custom__"] : selectedOptionKeys,
      highlighted: selectedOptionKeys[0] ?? (hasCustom ? "__custom__" : decision.options[0]?.key ?? null),
      notes: decision.response.notes ?? {},
      customLabel: decision.response.customLabel ?? (customDetail ? customOptionLabel(customDetail) : ""),
      customDetailMarkdown: customDetail,
      customOpen: hasCustom,
    };
  }
  if (decision.draft) {
    const selectedOptionKeys = decision.draft.selectedOptionKeys ?? [];
    const customDetail = decision.draft.customDetailMarkdown || decision.draft.customLabel || "";
    const customSelected = selectedOptionKeys.includes("__custom__");
    const hasCustomText = Boolean(customDetail.trim());
    return {
      selectedOptionKeys,
      highlighted: selectedOptionKeys[0] ?? (customSelected ? "__custom__" : decision.options[0]?.key ?? null),
      notes: decision.draft.notes ?? {},
      customLabel: decision.draft.customLabel ?? (customDetail ? customOptionLabel(customDetail) : ""),
      customDetailMarkdown: customDetail,
      customOpen: customSelected || hasCustomText,
    };
  }
  return {
    selectedOptionKeys: [],
    highlighted: decision.options[0]?.key ?? null,
    notes: {},
    customLabel: "",
    customDetailMarkdown: "",
    customOpen: false,
  };
}

export function isDecisionComplete(decision: PlanDecision, draft: PlanResponseInput): boolean {
  if (decision.mode === "dialogue") return true;
  if (draft.selectedOptionKeys.includes("__custom__") && !draft.customLabel.trim()) return false;
  const count = draft.selectedOptionKeys.filter((key) => key !== "__custom__").length
    + Number(draft.selectedOptionKeys.includes("__custom__") && Boolean(draft.customLabel.trim()));
  if (decision.requireSelection === false && count === 0) return true;
  return decision.mode === "single" ? count === 1 : count > 0;
}

export function PlanWorkspace({ id, changeToken }: { id: string; changeToken: string }) {
  const [detail, setDetail] = useState<PlanDetail | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [planOpen, setPlanOpen] = useState(false);
  const [stacked, setStacked] = useState(false);
  const [drafts, setDrafts] = useState<Record<number, DecisionDraft>>({});
  const [focusedDecisionId, setFocusedDecisionId] = useState<number | null>(null);
  const [message, setMessage] = useState("");
  const [busy, setBusy] = useState(false);

  const messagesRef = useRef<HTMLDivElement>(null);
  const saveDraftTimersRef = useRef<Map<number, ReturnType<typeof setTimeout>>>(new Map());

  const load = () => fetchPlan(id).then((value) => {
    setDetail(value);
    setError(null);
  }).catch((reason: unknown) => {
    setError(reason instanceof Error ? reason.message : String(reason));
  });

  useEffect(() => {
    const controller = new AbortController();
    fetchPlan(id, controller.signal).then(setDetail).catch((reason: unknown) => {
      if (!controller.signal.aborted) setError(reason instanceof Error ? reason.message : String(reason));
    });
    return () => controller.abort();
  }, [id, changeToken]);

  const activeDecisions = useMemo(() => {
    if (!detail) return null;
    const ids = detail.plan.activeDecisionIds?.length
      ? detail.plan.activeDecisionIds
      : detail.plan.activeDecisionId == null ? [] : [detail.plan.activeDecisionId];
    return ids.map((active) => detail.decisions.find((entry) => entry.id === active)).filter((entry): entry is PlanDecision => Boolean(entry));
  }, [detail]);

  const decision = activeDecisions?.find((entry) => entry.id === focusedDecisionId)
    ?? activeDecisions?.[0]
    ?? null;
  const draft = decision ? drafts[decision.id] ?? initialDraft(decision) : null;

  useEffect(() => {
    if (!activeDecisions?.length) return;
    const ids = activeDecisions.map((entry) => entry.id);
    setFocusedDecisionId((current) => resolveFocusedDecision(ids, current, readFocusedDecision(preferences, id)));
    setDrafts((current) => Object.fromEntries(activeDecisions.map((entry) => [entry.id, current[entry.id] ?? initialDraft(entry)])));
  }, [activeDecisions?.map((entry) => `${entry.id}:${entry.updatedAtUnixMs}`).join(",")]);

  const focusDecision = (decisionId: number) => {
    setFocusedDecisionId(decisionId);
    writeFocusedDecision(preferences, id, decisionId);
  };

  const thread = useMemo(() => {
    if (!detail) return [];
    return detail.messages.filter((entry) => (entry.decisionId ?? null) === (decision?.id ?? null));
  }, [detail, decision?.id]);

  useEffect(() => {
    const el = messagesRef.current;
    if (!el) return;
    el.scrollTop = el.scrollHeight;
  }, [thread]);

  useEffect(() => {
    return () => {
      for (const timer of saveDraftTimersRef.current.values()) {
        clearTimeout(timer);
      }
      saveDraftTimersRef.current.clear();
    };
  }, []);

  if (!detail) {
    return <div className="plan-loading">{error ?? "Loading plan…"}</div>;
  }

  const persistDraft = (decisionId: number, currentDraft: DecisionDraft, delayMs = 400) => {
    const existing = saveDraftTimersRef.current.get(decisionId);
    if (existing) clearTimeout(existing);
    if (delayMs === 0) {
      saveDraftTimersRef.current.delete(decisionId);
      savePlanDraft(id, decisionId, {
        selectedOptionKeys: currentDraft.selectedOptionKeys,
        customLabel: currentDraft.customLabel,
        customDetailMarkdown: currentDraft.customDetailMarkdown,
        notes: currentDraft.notes,
      }).catch(() => {});
      return;
    }
    const timer = setTimeout(() => {
      saveDraftTimersRef.current.delete(decisionId);
      savePlanDraft(id, decisionId, {
        selectedOptionKeys: currentDraft.selectedOptionKeys,
        customLabel: currentDraft.customLabel,
        customDetailMarkdown: currentDraft.customDetailMarkdown,
        notes: currentDraft.notes,
      }).catch(() => {});
    }, delayMs);
    saveDraftTimersRef.current.set(decisionId, timer);
  };

  const updateDraft = (decisionId: number, update: Partial<DecisionDraft>, immediate = false) => {
    setDrafts((current) => {
      const updated = { ...current[decisionId], ...update };
      persistDraft(decisionId, updated, immediate ? 0 : 400);
      return { ...current, [decisionId]: updated };
    });
  };

  const choose = (key: string) => {
    if (!decision || !draft || decision.state === "waiting") return;
    const hasCustomText = draft.customDetailMarkdown.trim().length > 0;
    updateDraft(decision.id, {
      highlighted: key,
      selectedOptionKeys: nextPlanSelection(decision.mode, draft.selectedOptionKeys, key),
      customOpen: hasCustomText ? draft.customOpen : false,
    }, true);
  };

  const handleCustomText = (text: string) => {
    if (!decision || !draft || decision.state === "waiting") return;
    const result = updateCustomTextDraft(decision.mode, draft.selectedOptionKeys, text);
    updateDraft(decision.id, {
      ...result,
      highlighted: "__custom__",
    }, false);
  };

  const handleCustomHeadClick = () => {
    if (!decision || !draft || decision.state === "waiting") return;
    const hasText = draft.customDetailMarkdown.trim().length > 0;
    const result = toggleCustomOption(decision.mode, draft.selectedOptionKeys, hasText);
    updateDraft(decision.id, {
      ...result,
      highlighted: "__custom__",
    }, true);
  };

  const submit = () => {
    if (!activeDecisions?.length) return;
    for (const timer of saveDraftTimersRef.current.values()) {
      clearTimeout(timer);
    }
    saveDraftTimersRef.current.clear();
    setBusy(true);
    const responses = activeDecisions.map((entry) => {
      const value = drafts[entry.id];
      const customSelected = value.selectedOptionKeys.includes("__custom__");
      return { decisionId: entry.id, response: {
        selectedOptionKeys: value.selectedOptionKeys.filter((key) => key !== "__custom__"),
        customLabel: customSelected ? value.customLabel : "",
        customDetailMarkdown: customSelected ? value.customDetailMarkdown : "",
        notes: value.notes,
      } };
    });
    setDetail((current) => current && ({ ...current, decisions: current.decisions.map((entry) =>
      activeDecisions.some((active) => active.id === entry.id) ? { ...entry, state: "waiting" } : entry) }));
    submitPlanDecisions(id, responses).then(load).catch((reason: unknown) => {
      setError(reason instanceof Error ? reason.message : String(reason));
      return load();
    }).finally(() => setBusy(false));
  };

  const sendMessage = () => {
    const body = message.trim();
    if (!body) return;
    setBusy(true);
    postPlanMessage(id, decision?.id ?? null, body).then(() => {
      setMessage("");
      return load();
    }).catch((reason: unknown) => {
      setError(reason instanceof Error ? reason.message : String(reason));
    }).finally(() => setBusy(false));
  };

  const handleComposerKeyDown = (event: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (event.key === "Enter" && !event.shiftKey) {
      event.preventDefault();
      sendMessage();
    }
  };

  const highlightedOption = decision?.options.find((option) => option.key === draft?.highlighted);
  const waiting = decision?.state === "waiting";
  const focusedIndex = activeDecisions?.findIndex((entry) => entry.id === decision?.id) ?? -1;
  const batch: BatchEntry[] = activeDecisions?.map((entry) => ({
    id: entry.id,
    title: entry.title,
    complete: isDecisionComplete(entry, drafts[entry.id] ?? initialDraft(entry)),
  })) ?? [];
  const complete = batch.every((entry) => entry.complete);

  return (
    <section className={`plan-workspace ${stacked ? "is-stacked" : ""}`}>
      <header className="plan-workspace-head">
        <div>
          <span className="plan-eyebrow">{detail.plan.state} plan</span>
          <strong>{detail.plan.name}</strong>
          {detail.plan.summary && <span>{detail.plan.summary}</span>}
        </div>
        <button
          className="btn plan-layout-toggle"
          type="button"
          aria-label={stacked ? "Use side-by-side layout" : "Use stacked layout"}
          title={stacked ? "Use side-by-side layout" : "Use stacked layout"}
          onClick={() => setStacked((value) => !value)}
        >
          <svg viewBox="0 0 16 16" aria-hidden="true">
            {stacked ? <path d="M2 3h12v4H2zM2 9h12v4H2z" /> : <path d="M2 3h5v10H2zM9 3h5v10H9z" />}
          </svg>
        </button>
      </header>
      {error && <div className="flash-error" role="alert"><span>{error}</span><button className="btn" type="button" onClick={() => setError(null)}>dismiss</button></div>}
      <div className="plan-workspace-body">
        {decision && (
          <aside className="plan-context-rail">
            <button
              className={`plan-rail-btn ${planOpen ? "is-active" : ""}`}
              type="button"
              aria-label={planOpen ? "Show decision" : "Show plan context"}
              title={planOpen ? "Show decision" : `Plan context (rev ${detail.plan.revision})`}
              onClick={() => setPlanOpen((value) => !value)}
            >
              <svg viewBox="0 0 16 16" aria-hidden="true">
                <path d="M3 2h7l3 3v9H3zM9 2v4h4" />
              </svg>
              <span className="plan-rail-label">Plan context{detail.plan.revision ? ` (rev ${detail.plan.revision})` : ""}</span>
            </button>
          </aside>
        )}
        <main className="plan-focus">
          {planOpen && decision ? (
            <div className="plan-context-view">
              <div className="plan-context-view-head">
                <div>
                  <span className="plan-eyebrow">Plan context</span>
                  <h2>{detail.plan.name} {detail.plan.revision ? `(rev ${detail.plan.revision})` : ""}</h2>
                </div>
                <button
                  className="btn btn-primary plan-return-btn"
                  type="button"
                  onClick={() => setPlanOpen(false)}
                >
                  ← Back to decision
                </button>
              </div>
              <div className="plan-document">
                <Markdown text={detail.markdown} onPmLink={() => undefined} />
              </div>
            </div>
          ) : decision ? (
            <DecisionPanel
              decision={decision}
              waiting={waiting}
              selected={draft?.selectedOptionKeys ?? []}
              highlighted={draft?.highlighted ?? null}
              highlightedDetail={draft?.highlighted === "__custom__" ? (draft.customDetailMarkdown.trim() ? draft.customDetailMarkdown : "Describe your custom option in the text area.") : highlightedOption?.detailMarkdown ?? decision.detailMarkdown}
              customOpen={draft?.customOpen ?? false}
              customText={draft?.customDetailMarkdown ?? ""}
              notes={draft?.notes ?? {}}
              busy={busy}
              batch={batch}
              batchSize={batch.length}
              batchIndex={focusedIndex}
              batchComplete={complete}
              onChoose={choose}
              onHighlight={(key) => updateDraft(decision.id, { highlighted: key })}
              onCustomHeadClick={handleCustomHeadClick}
              onCustomText={handleCustomText}
              onNote={(key, value) => updateDraft(decision.id, { notes: { ...(draft?.notes ?? {}), [key]: value } })}
              onPrevious={() => focusedIndex > 0 && focusDecision(batch[focusedIndex - 1].id)}
              onNext={() => focusedIndex + 1 < batch.length && focusDecision(batch[focusedIndex + 1].id)}
              onJump={(index) => batch[index] && focusDecision(batch[index].id)}
              onSubmit={submit}
            />
          ) : (
            <div className="plan-document"><Markdown text={detail.markdown} onPmLink={() => undefined} /></div>
          )}
        </main>
        <aside className="plan-dialogue">
          <header><span>{decision ? "Decision dialogue" : "Plan dialogue"}</span><small>live with agent</small></header>
          <div className="plan-messages" ref={messagesRef}>
            {thread.length === 0 && <p className="muted-line">Ask a question or refine the approach with the agent.</p>}
            {thread.map((entry) => (
              <article className={`plan-message is-${entry.author}`} key={entry.id}>
                <small>{entry.author === "session" ? "Agent" : "You"}</small>
                <Markdown text={entry.body} onPmLink={() => undefined} />
              </article>
            ))}
          </div>
          <div className="plan-composer">
            <textarea
              value={message}
              onChange={(event) => setMessage(event.target.value)}
              onKeyDown={handleComposerKeyDown}
              placeholder="Message the agent… (Enter to send, Shift+Enter for newline)"
              rows={2}
            />
            <div className="plan-composer-hint">
              <span>Enter to send</span>
              <span>Shift+Enter for newline</span>
            </div>
          </div>
        </aside>
      </div>
    </section>
  );
}

export function DecisionPanel({
  decision, waiting, selected, highlighted, highlightedDetail, customOpen,
  customText, notes, busy, onChoose, onHighlight, onCustomHeadClick, onCustomText,
  onNote, batch, batchSize, batchIndex, batchComplete, onPrevious, onNext, onJump, onSubmit,
}: {
  decision: PlanDecision;
  waiting: boolean;
  selected: string[];
  highlighted: string | null;
  highlightedDetail: string;
  customOpen: boolean;
  customText: string;
  notes: Record<string, string>;
  busy: boolean;
  batchSize: number;
  batch: BatchEntry[];
  batchIndex: number;
  batchComplete: boolean;
  onChoose: (key: string) => void;
  onHighlight: (key: string) => void;
  onCustomHeadClick: () => void;
  onCustomText: (value: string) => void;
  onNote: (key: string, value: string) => void;
  onPrevious: () => void;
  onNext: () => void;
  onJump: (index: number) => void;
  onSubmit: () => void;
}) {
  const customTextareaRef = useRef<HTMLTextAreaElement>(null);
  const isCustomSelected = selected.includes("__custom__");

  const handleHeadClick = () => {
    onCustomHeadClick();
    setTimeout(() => {
      customTextareaRef.current?.focus();
    }, 0);
  };

  return <div className="plan-decision">
    <header className="plan-decision-head">
      <div>
        {batchSize > 1 && <nav className="plan-batch-nav" aria-label="Decisions in this batch">
          <span className="plan-eyebrow">{`Decision ${batchIndex + 1} of ${batchSize}`}</span>
          <ol className="plan-batch-steps">
            {batch.map((entry, index) => {
              const isCurrent = index === batchIndex;
              return <li key={entry.id}>
                <button
                  type="button"
                  className={["plan-batch-step", isCurrent && "is-current", entry.complete && "is-complete"].filter(Boolean).join(" ")}
                  aria-current={isCurrent ? "step" : undefined}
                  aria-label={`Decision ${index + 1}: ${entry.title}${entry.complete ? " (answered)" : ""}`}
                  title={entry.title}
                  onClick={() => onJump(index)}
                >{index + 1}</button>
              </li>;
            })}
          </ol>
        </nav>}
        <h1>{decision.title}</h1>
      </div>
      {waiting && <span className="plan-decision-state is-waiting">Waiting for agent</span>}
    </header>
    {decision.promptMarkdown && <Markdown text={decision.promptMarkdown} onPmLink={() => undefined} />}
    <div className="plan-decision-grid">
      <div className="plan-options" role={decision.mode === "multiple" ? "group" : "radiogroup"} aria-label={decision.title}>
        {decision.options.map((option) => {
          const checked = selected.includes(option.key);
          const isRecommended = decision.mode === "single" && Boolean(option.recommended);
          return <button
            className={`plan-option ${highlighted === option.key ? "is-highlighted" : ""} ${checked ? "is-selected" : ""}`}
            type="button"
            key={option.key}
            disabled={waiting}
            onClick={() => onChoose(option.key)}
          >
            <span className={decision.mode === "multiple" ? "plan-checkbox" : "plan-radio"} aria-hidden="true">{checked ? "✓" : ""}</span>
            <span>
              <span className="plan-option-label-row">
                <strong>{option.label}</strong>
                {isRecommended && <span className="plan-recommended-badge">Recommended</span>}
              </span>
              <small>{option.key}</small>
            </span>
          </button>;
        })}
        {decision.allowCustom && (
          <div
            className={`plan-option plan-custom-option ${highlighted === "__custom__" ? "is-highlighted" : ""} ${isCustomSelected ? "is-selected" : ""}`}
            onClick={() => {
              onHighlight("__custom__");
              handleHeadClick();
            }}
          >
            <div
              className="plan-custom-head"
              role={decision.mode === "multiple" ? "checkbox" : "radio"}
              aria-checked={isCustomSelected}
              tabIndex={waiting ? -1 : 0}
              onClick={(event) => {
                event.stopPropagation();
                handleHeadClick();
              }}
              onKeyDown={(event) => {
                if (event.key === "Enter" || event.key === " ") {
                  event.preventDefault();
                  event.stopPropagation();
                  handleHeadClick();
                }
              }}
            >
              <span className={decision.mode === "multiple" ? "plan-checkbox" : "plan-radio"} aria-hidden="true">
                {isCustomSelected ? "✓" : ""}
              </span>
              <span className="plan-custom-title">
                <strong>Add your own option</strong>
              </span>
            </div>
            {(customOpen || customText.length > 0) && (
              <div className="plan-custom-body">
                <textarea
                  ref={customTextareaRef}
                  autoFocus
                  rows={3}
                  className="plan-custom-textarea"
                  value={customText}
                  disabled={waiting}
                  placeholder="Describe your option…"
                  onClick={(event) => event.stopPropagation()}
                  onFocus={() => onHighlight("__custom__")}
                  onChange={(event) => onCustomText(event.target.value)}
                />
              </div>
            )}
          </div>
        )}
      </div>
      <div className="plan-option-detail">
        <span className="plan-eyebrow">Selected option details</span>
        <Markdown text={highlightedDetail || "Select an option to see its details."} onPmLink={() => undefined} />
        {highlighted && !waiting && <label className="plan-option-note">Note for this option<textarea rows={3} value={notes[highlighted] ?? ""} onChange={(event) => onNote(highlighted, event.target.value)} placeholder="Optional context for the agent…" /></label>}
      </div>
    </div>
    {decision.mode !== "dialogue" && <footer className="plan-submit-bar">
      <span>{waiting ? "Answers submitted. Dialogue remains open." : batchSize > 1 ? "Nothing is sent until you submit the whole batch." : "Review your selection and notes before sending."}</span>
      <div className="plan-batch-actions">
        {batchSize > 1 && <button className="btn" type="button" disabled={batchIndex === 0} onClick={onPrevious}>Back</button>}
        {batchSize > 1 && batchIndex < batchSize - 1 && <button className="btn btn-primary" type="button" onClick={onNext}>Next</button>}
        {batchIndex === batchSize - 1 && <button className="btn btn-primary plan-submit" type="button" disabled={waiting || busy || !batchComplete} onClick={onSubmit}>{batchSize > 1 ? "SUBMIT ALL" : "SUBMIT DECISION"} <span aria-hidden="true">↗</span></button>}
      </div>
    </footer>}
  </div>;
}
