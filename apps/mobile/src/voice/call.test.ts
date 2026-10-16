import { describe, expect, it } from "vitest";

import { BARGE_IN_WORDS, CallTurns, looksLikeEcho } from "./call";

describe("taking turns in a call", () => {
  it("sends what it hears while it is listening", () => {
    const turns = new CallTurns();
    expect(turns.heard("what needs me")).toEqual({ kind: "send", text: "what needs me" });
    expect(turns.at()).toBe("working");
  });

  it("ignores silence dressed up as an utterance", () => {
    const turns = new CallTurns();
    expect(turns.heard("   ")).toEqual({ kind: "ignored" });
    expect(turns.at()).toBe("listening");
  });

  /**
   * Echo cancellation is good, not perfect. A syllable of the agent's
   * own voice coming back must not cut it off, so an interruption has
   * to be long enough to be a person deciding to speak.
   */
  it("takes enough words as an interruption and a stray one as leakage", () => {
    const turns = new CallTurns();
    turns.speaking();
    expect(turns.heard("the")).toEqual({ kind: "ignored" });
    expect(turns.at()).toBe("speaking");

    expect(turns.heard("stop that")).toEqual({ kind: "interrupt", text: "stop that" });
    expect(turns.at()).toBe("working");
    expect(BARGE_IN_WORDS).toBe(2);
  });

  /**
   * Held rather than sent, and joined rather than queued one by one:
   * separate turns come back as answers to questions the user has
   * stopped waiting for.
   */
  it("holds what it hears while the agent works and sends it as one turn", () => {
    const turns = new CallTurns();
    turns.heard("what needs me");
    expect(turns.heard("and the board")).toEqual({ kind: "queued" });
    expect(turns.heard("and session five")).toEqual({ kind: "queued" });

    expect(turns.idle()).toBe("and the board and session five");
    expect(turns.at()).toBe("working");
  });

  it("goes back to listening when nothing was waiting", () => {
    const turns = new CallTurns();
    turns.heard("what needs me");
    expect(turns.idle()).toBeNull();
    expect(turns.at()).toBe("listening");
  });

  /// Two answers in a row must not replay the first one's leftovers.
  it("sends what it held only once", () => {
    const turns = new CallTurns();
    turns.heard("what needs me");
    turns.heard("and the board");
    expect(turns.idle()).toBe("and the board");
    expect(turns.idle()).toBeNull();
  });

  /**
   * Interrupting while something is still waiting used to leave it
   * behind, and the stopped answer put the call back to idle, which
   * sent it as a turn of its own alongside the interruption.
   */
  it("takes what it held with an interruption rather than sending it after", () => {
    const turns = new CallTurns();
    turns.heard("what needs me");
    turns.heard("and the board");
    turns.speaking();
    const action = turns.heard("stop, tell me about session five");

    expect(action).toEqual({
      kind: "interrupt",
      text: "and the board stop, tell me about session five",
    });
    expect(turns.idle()).toBeNull();
  });

  it("forgets what was waiting when the call restarts", () => {
    const turns = new CallTurns();
    turns.heard("what needs me");
    turns.heard("and the board");
    turns.reset();
    expect(turns.at()).toBe("listening");
    expect(turns.idle()).toBeNull();
  });
});

describe("the speaker leaking into the microphone", () => {
  /**
   * On the speaker the microphone hears the answer. Echo cancellation
   * leaves enough of it to pass the barge-in threshold, so without this
   * the agent is asked about the sentence it just read out.
   */
  it("ignores the answer coming back word for word", () => {
    const turns = new CallTurns();
    turns.spoke("Checking the phone on bench one now, it is about halfway through.");
    turns.working();
    turns.speaking();

    expect(turns.heard("checking the phone on bench one now")).toEqual({ kind: "ignored" });
    expect(turns.at()).toBe("speaking");
  });

  /**
   * Playback outlasts the answer the daemon finished sending, so the
   * tail of it arrives once the call is listening again, where nothing
   * else would question it.
   */
  it("ignores the tail that arrives after the answer is over", () => {
    const turns = new CallTurns();
    turns.spoke("It is about halfway through.");
    expect(turns.at()).toBe("listening");
    expect(turns.heard("it is about halfway through")).toEqual({ kind: "ignored" });
  });

  it("still hears the user over the answer", () => {
    const turns = new CallTurns();
    turns.spoke("Checking the phone on bench one now.");
    turns.speaking();

    expect(turns.heard("stop that and look at the other one")).toEqual({
      kind: "interrupt",
      text: "stop that and look at the other one",
    });
  });

  // Short answers to the agent's own question share its words, so they
  // are left to the barge-in threshold rather than scored for echo.
  it("does not drop a short reply that repeats a word of the question", () => {
    const turns = new CallTurns();
    turns.spoke("Do you want me to go ahead, yes or no?");
    expect(turns.heard("yes go")).toEqual({ kind: "send", text: "yes go" });
  });

  it("forgets what was said when the call ends", () => {
    const turns = new CallTurns();
    turns.spoke("Checking the phone on bench one now.");
    turns.reset();
    expect(turns.heard("checking the phone on bench one now")).toEqual({
      kind: "send",
      text: "checking the phone on bench one now",
    });
  });
});

describe("looksLikeEcho", () => {
  /**
   * Recognition does not give speech back as it was written, so the
   * comparison is on short runs of characters: a misheard word costs a
   * few runs rather than the whole match.
   */
  it("still matches speech that came back misheard", () => {
    const spoken = "The phone on bench one is about halfway through.";
    expect(looksLikeEcho("the phone on bench one is about halfway through", spoken)).toBe(true);
    expect(looksLikeEcho("the fone on bench won is about half way through", spoken)).toBe(true);
    expect(looksLikeEcho("phone bench one about halfway", spoken)).toBe(true);
  });

  it("does not match something else said about the same thing", () => {
    const spoken = "The phone on bench one is about halfway through.";
    expect(looksLikeEcho("start the other one over", spoken)).toBe(false);
    expect(looksLikeEcho("what did the second run say", spoken)).toBe(false);
  });

  it("needs something to compare, and enough of it", () => {
    expect(looksLikeEcho("the phone is on bench one", "")).toBe(false);
    expect(looksLikeEcho("", "The phone is on bench one.")).toBe(false);
    // Two words are left to the barge-in threshold, since a short reply
    // to the agent's own question is made of its words.
    expect(looksLikeEcho("halfway through", "It is about halfway through.")).toBe(false);
  });
});
