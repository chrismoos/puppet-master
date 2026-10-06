import { describe, expect, it } from "vitest";
import { markdownChoices } from "../components/markdownChoice";
import {
  answerIsSendable,
  buildChoiceAnswer,
  choiceAnswerBody,
  choiceAnswerStatus,
  chosenOptionIds,
  toggleChoice,
  type ChoiceAnswer,
} from "./reviewChoice";

const DOC = [
  "<!-- pm-choice id=auth-approach select=one -->",
  "- [ ] JWT with refresh rotation",
  "- [ ] Server-side sessions",
  "- [ ] Other",
].join("\n");

const choice = () => markdownChoices(DOC)[0];

const answer = (over: Partial<ChoiceAnswer> = {}): ChoiceAnswer => ({
  choice_id: "auth-approach",
  select: "one",
  option_ids: ["server-side-sessions"],
  option_labels: ["Server-side sessions"],
  other_text: "",
  notes: "",
  ...over,
});

describe("an answer against a revised document", () => {
  it("stands when the block and the options are unchanged", () => {
    expect(choiceAnswerStatus(choice(), answer(), "same")).toBe("same");
  });

  // The whole point of anchoring: the plan grew above the question and
  // the answer follows the question rather than the line number.
  it("follows the block when it only moved", () => {
    expect(choiceAnswerStatus(choice(), answer(), "moved")).toBe("moved");
  });

  it("goes stale when the chosen option is no longer offered", () => {
    const reworded = markdownChoices(
      [
        "<!-- pm-choice id=auth-approach select=one -->",
        "- [ ] JWT with refresh rotation",
        "- [ ] Sessions held on the server",
      ].join("\n"),
    )[0];
    expect(choiceAnswerStatus(reworded, answer(), "same")).toBe("stale");
  });

  // An option nobody touched is still the same option, so adding one
  // must not throw away a decision that is still valid.
  it("stands when an option is added beside the chosen one", () => {
    const widened = markdownChoices(
      [
        "<!-- pm-choice id=auth-approach select=one -->",
        "- [ ] JWT with refresh rotation",
        "- [ ] Server-side sessions",
        "- [ ] Signed cookies",
        "- [ ] Other",
      ].join("\n"),
    )[0];
    expect(choiceAnswerStatus(widened, answer(), "same")).toBe("same");
  });

  it("goes stale when the block itself was edited or cannot be found", () => {
    expect(choiceAnswerStatus(choice(), answer(), "changed")).toBe("stale");
    expect(choiceAnswerStatus(choice(), answer(), "unknown")).toBe("stale");
  });

  it("goes stale when the question turned into a different kind of question", () => {
    const many = markdownChoices(DOC.replace("select=one", "select=many"))[0];
    expect(choiceAnswerStatus(many, answer(), "same")).toBe("stale");
  });
});

describe("what the document shows as chosen", () => {
  it("marks the answered options so the plan records its own decision", () => {
    expect([...chosenOptionIds(choice(), answer(), "same")]).toEqual([
      "server-side-sessions",
    ]);
  });

  // A stale answer must not paint a tick on an option nobody chose.
  it("shows nothing chosen once the answer has gone stale", () => {
    expect([...chosenOptionIds(choice(), answer(), "stale")]).toEqual([]);
  });

  it("falls back to the ticks the document itself carries", () => {
    const preticked = markdownChoices(
      ["<!-- pm-choice id=a -->", "- [ ] no", "- [x] yes"].join("\n"),
    )[0];
    expect([...chosenOptionIds(preticked, null, null)]).toEqual(["yes"]);
  });
});

describe("picking options", () => {
  it("replaces the selection when the marker asked for one", () => {
    expect([...toggleChoice("one", new Set(["a"]), "b")]).toEqual(["b"]);
    expect([...toggleChoice("one", new Set(["b"]), "b")]).toEqual(["b"]);
  });

  it("toggles within the selection when the marker asked for many", () => {
    expect([...toggleChoice("many", new Set(["a"]), "b")]).toEqual(["a", "b"]);
    expect([...toggleChoice("many", new Set(["a", "b"]), "b")]).toEqual(["a"]);
  });
});

describe("the answer that travels", () => {
  it("carries ids and the labels the reader saw", () => {
    const built = buildChoiceAnswer(
      choice(),
      new Set(["server-side-sessions"]),
      "",
      "  sticky routing is fine  ",
    );
    expect(built).toEqual({
      choice_id: "auth-approach",
      select: "one",
      option_ids: ["server-side-sessions"],
      option_labels: ["Server-side sessions"],
      other_text: "",
      notes: "sticky routing is fine",
    });
  });

  it("lists the options in document order, not in the order they were clicked", () => {
    const many = markdownChoices(DOC.replace("select=one", "select=many"))[0];
    const built = buildChoiceAnswer(many, new Set(["other", "jwt-with-refresh-rotation"]), "mTLS", "");
    expect(built.option_ids).toEqual(["jwt-with-refresh-rotation", "other"]);
    expect(built.other_text).toBe("mTLS");
  });

  it("reads as a plain decision for whoever opens the thread", () => {
    expect(choiceAnswerBody(answer({ notes: "revocation matters" }))).toBe(
      "auth-approach: Server-side sessions\nrevocation matters",
    );
    expect(
      choiceAnswerBody(
        answer({ option_ids: ["other"], option_labels: ["Other"], other_text: "mTLS" }),
      ),
    ).toBe("auth-approach: Other\nother: mTLS");
  });
});

describe("when an answer may be sent", () => {
  it("needs something chosen", () => {
    expect(answerIsSendable(choice(), new Set(), "")).toBe(false);
    expect(answerIsSendable(choice(), new Set(["server-side-sessions"]), "")).toBe(true);
  });

  // "Other" with nothing written in it tells the agent nothing.
  it("needs the free-text option to actually say something", () => {
    expect(answerIsSendable(choice(), new Set(["other"]), "   ")).toBe(false);
    expect(answerIsSendable(choice(), new Set(["other"]), "mTLS")).toBe(true);
  });
});
