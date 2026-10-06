import type { ChoiceList, ChoiceOption } from "../components/markdownChoice";
import type { AnchorStatus } from "./reviewDiff";

// An answer to a marked option list is anchored exactly the way a
// comment is, so a review revised mid-pass carries it along. What a
// comment cannot go stale against is the thing it is attached to: the
// options themselves can be rewritten under an answer, and re-reading
// that answer against the new set would silently move a decision onto
// an option nobody chose.

/** The answer as it travels to the agent and comes back on a message. */
export interface ChoiceAnswer {
  choice_id: string;
  select: "one" | "many";
  option_ids: string[];
  option_labels: string[];
  other_text: string;
  notes: string;
}

/**
 * Where an answer stands against the document as it reads now.
 *
 * `stale` means the question changed under the answer, so the reader is
 * asked again rather than having their decision reattached.
 */
export type ChoiceAnswerStatus = "same" | "moved" | "stale";

export function choiceAnswerStatus(
  choice: ChoiceList,
  answer: ChoiceAnswer,
  anchor: AnchorStatus,
): ChoiceAnswerStatus {
  // The block the answer was written against was itself edited, so what
  // it agreed to is no longer on the page.
  if (anchor === "changed" || anchor === "unknown") return "stale";
  const present = new Set(choice.options.map((o) => o.id));
  // An option is identified by its text, so a reworded label is a
  // different option and an answer naming the old one is out of date.
  if (answer.option_ids.some((id) => !present.has(id))) return "stale";
  // The list changed from one answer to several, or the other way, so
  // what the answer means has changed even if the options have not.
  if (answer.select !== choice.select) return "stale";
  return anchor === "moved" ? "moved" : "same";
}

/** What the reader has chosen, ready to render as ticks in the
 *  document, and empty for an answer that no longer fits the list. */
export function chosenOptionIds(
  choice: ChoiceList,
  answer: ChoiceAnswer | null,
  status: ChoiceAnswerStatus | null,
): Set<string> {
  if (!answer || status === "stale") {
    // With no answer the document speaks for itself, ticks included.
    return new Set(choice.options.filter((o) => o.checked).map((o) => o.id));
  }
  return new Set(answer.option_ids);
}

/** Applies a click to a selection, honouring what the marker asked for:
 *  one option replaces the selection, many toggles within it. */
export function toggleChoice(
  select: ChoiceList["select"],
  chosen: ReadonlySet<string>,
  optionId: string,
): Set<string> {
  if (select === "one") return new Set([optionId]);
  const next = new Set(chosen);
  if (next.has(optionId)) next.delete(optionId);
  else next.add(optionId);
  return next;
}

export function buildChoiceAnswer(
  choice: ChoiceList,
  chosen: ReadonlySet<string>,
  otherText: string,
  notes: string,
): ChoiceAnswer {
  const picked = choice.options.filter((o) => chosen.has(o.id));
  return {
    choice_id: choice.id,
    select: choice.select,
    option_ids: picked.map((o) => o.id),
    option_labels: picked.map((o) => o.label),
    other_text: otherText.trim(),
    notes: notes.trim(),
  };
}

/**
 * The line a person reads on the thread. The agent reads the fields, so
 * this only has to say plainly what was decided.
 */
export function choiceAnswerBody(answer: ChoiceAnswer): string {
  const chose = answer.option_labels.length
    ? answer.option_labels.join(", ")
    : "nothing";
  const lines = [`${answer.choice_id}: ${chose}`];
  if (answer.other_text) lines.push(`other: ${answer.other_text}`);
  if (answer.notes) lines.push(answer.notes);
  return lines.join("\n");
}

/** An answer is only complete once the free-text option has text. */
export function answerIsSendable(
  choice: ChoiceList,
  chosen: ReadonlySet<string>,
  otherText: string,
): boolean {
  if (chosen.size === 0) return false;
  const other = choice.options.find((o) => isOther(o) && chosen.has(o.id));
  return !other || otherText.trim() !== "";
}

function isOther(option: ChoiceOption): boolean {
  return option.id === "other";
}
