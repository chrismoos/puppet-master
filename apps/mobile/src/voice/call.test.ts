import { describe, expect, it } from "vitest";

import { BARGE_IN_WORDS, CallTurns } from "./call";

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
