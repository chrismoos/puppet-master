import { useEffect, useId, useState } from "react";
import type { ChoiceList } from "../components/markdownChoice";
import {
  answerIsSendable,
  buildChoiceAnswer,
  choiceAnswerStatus,
  chosenOptionIds,
  toggleChoice,
  type ChoiceAnswer,
  type ChoiceAnswerStatus,
} from "./reviewChoice";
import type { AnchorStatus } from "./reviewDiff";

export interface ChoiceCardProps {
  choice: ChoiceList;
  /** The answer already recorded against this question, if any. */
  answer: ChoiceAnswer | null;
  /** Where the answer's thread is: unsent, with the agent, or closed. */
  threadState: "draft" | "sent" | "answered" | "resolved" | null;
  /** How the answer's own block fared in the revision being read. */
  anchor: AnchorStatus;
  onAnswer: (answer: ChoiceAnswer) => void;
  onSend: () => void;
}

const OTHER_ID = "other";

export function ChoiceCard({
  choice,
  answer,
  threadState,
  anchor,
  onAnswer,
  onSend,
}: ChoiceCardProps) {
  // Two files in one review can carry the same question, and radios
  // sharing a name are one group however far apart they render.
  const group = useId();
  const status: ChoiceAnswerStatus | null = answer
    ? choiceAnswerStatus(choice, answer, anchor)
    : null;
  const stale = status === "stale";
  // Once the agent has it, the answer is a record rather than a control.
  const settled = threadState !== null && threadState !== "draft" && !stale;

  const [chosen, setChosen] = useState<Set<string>>(() =>
    chosenOptionIds(choice, answer, status),
  );
  const [otherText, setOtherText] = useState(stale ? "" : (answer?.other_text ?? ""));
  const [notes, setNotes] = useState(stale ? "" : (answer?.notes ?? ""));

  // What the review holds is the source of truth on arrival, so a reload
  // or a refresh after sending cannot leave the card showing a decision
  // the review does not have. Keyed on the answer itself rather than on
  // the objects around it, or reseeding would fight the reader's typing.
  const recorded = answer ? `${answer.option_ids.join(" ")}|${answer.other_text}|${answer.notes}` : "";
  useEffect(() => {
    setChosen(chosenOptionIds(choice, answer, status));
    setOtherText(stale ? "" : (answer?.other_text ?? ""));
    setNotes(stale ? "" : (answer?.notes ?? ""));
  }, [recorded, stale]);

  const answered = chosen.size > 0;
  const sendable = answerIsSendable(choice, chosen, otherText) && !settled;

  const emit = (next: Set<string>, other: string, note: string) => {
    if (next.size === 0) return;
    onAnswer(buildChoiceAnswer(choice, next, other, note));
  };

  const pick = (optionId: string) => {
    if (settled) return;
    const next = toggleChoice(choice.select, chosen, optionId);
    setChosen(next);
    emit(next, otherText, notes);
  };

  return (
    <div className={`review-choice ${stale ? "is-stale" : ""} ${settled ? "is-settled" : ""}`}>
      <div className="review-choice-head">
        <span className="review-choice-id">{choice.id}</span>
        <span className="review-choice-mode">
          {choice.select === "many" ? "choose any" : "choose one"}
        </span>
        {settled && <span className="review-choice-state">{threadState}</span>}
        {sendable && (
          <button
            type="button"
            className="review-choice-send"
            title="Send this answer now"
            aria-label="send this answer now"
            onClick={onSend}
          >
            {"➤"}
          </button>
        )}
      </div>

      {stale && (
        <p className="review-choice-stale">
          the options changed since this was answered, so it needs answering again
        </p>
      )}

      <ul className="review-choice-options">
        {choice.options.map((option) => {
          const picked = chosen.has(option.id);
          return (
            <li key={option.id} className={picked ? "is-picked" : ""}>
              <label>
                <input
                  type={choice.select === "many" ? "checkbox" : "radio"}
                  name={group}
                  checked={picked}
                  disabled={settled}
                  onChange={() => pick(option.id)}
                />
                <span className="review-choice-label">{option.label}</span>
              </label>
              {option.detail !== "" && (
                <p className="review-choice-detail">{option.detail}</p>
              )}
              {option.id === OTHER_ID && picked && (
                <input
                  type="text"
                  className="review-choice-other"
                  placeholder="What instead?"
                  aria-label="the other option"
                  value={otherText}
                  disabled={settled}
                  onChange={(e) => {
                    setOtherText(e.target.value);
                    emit(chosen, e.target.value, notes);
                  }}
                />
              )}
            </li>
          );
        })}
      </ul>

      {answered && (
        <textarea
          className="review-choice-notes"
          rows={2}
          placeholder="Notes for the agent"
          aria-label={`notes on ${choice.id}`}
          value={notes}
          disabled={settled}
          onChange={(e) => {
            setNotes(e.target.value);
            emit(chosen, otherText, e.target.value);
          }}
        />
      )}
    </div>
  );
}
