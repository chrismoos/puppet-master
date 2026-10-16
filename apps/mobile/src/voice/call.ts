/**
 * Turn-taking for a call, where the microphone stays open and the user
 * is never pressing anything.
 *
 * The rules are here rather than in the screen because they are where
 * the mistakes are: what to do with words heard while the agent is
 * still working, and what counts as interrupting it rather than as the
 * speaker's own voice leaking back through the microphone.
 */

/** What the call is doing, which decides what an utterance means. */
export type CallPhase = "listening" | "working" | "speaking";

export type TurnAction =
  | { kind: "send"; text: string }
  | { kind: "interrupt"; text: string }
  | { kind: "queued" }
  | { kind: "ignored" };

/**
 * How many words make an interruption. Echo cancellation is good, not
 * perfect, so a syllable of the agent's own voice coming back must not
 * cut it off mid-sentence.
 */
export const BARGE_IN_WORDS = 2;

function words(text: string): number {
  return text.trim().split(/\s+/).filter(Boolean).length;
}

export class CallTurns {
  private phase: CallPhase = "listening";
  /**
   * Heard while the agent was busy. Joined into one turn rather than
   * sent one after another: separate turns arrive as answers to
   * questions the user has stopped waiting for, and the agent reads
   * them together with the exchange that came before.
   */
  private held: string[] = [];

  at(): CallPhase {
    return this.phase;
  }

  /** The agent took the turn, so nothing said now is a new one. */
  working(): void {
    this.phase = "working";
  }

  /** The answer is being read out, which is interruptible. */
  speaking(): void {
    this.phase = "speaking";
  }

  /** A finished utterance, and what the call should do about it. */
  heard(text: string): TurnAction {
    const said = text.trim();
    if (!said) return { kind: "ignored" };
    switch (this.phase) {
      case "listening":
        this.phase = "working";
        return { kind: "send", text: said };
      case "speaking":
        if (words(said) < BARGE_IN_WORDS) return { kind: "ignored" };
        this.phase = "working";
        // Anything held while the agent worked goes with it. Left
        // behind it would be sent again the moment the interrupted
        // answer stops, as a turn of its own.
        return { kind: "interrupt", text: this.drain(said) };
      case "working":
        this.held.push(said);
        return { kind: "queued" };
    }
  }

  /**
   * The agent has finished. Returns everything held while it worked, as
   * one utterance, or null when there is nothing waiting and the call
   * goes back to listening.
   */
  idle(): string | null {
    if (this.held.length === 0) {
      this.phase = "listening";
      return null;
    }
    this.phase = "working";
    return this.drain();
  }

  /** Everything waiting, as one utterance, with `last` spoken most recently. */
  private drain(last?: string): string {
    const together = [...this.held, ...(last ? [last] : [])].join(" ");
    this.held = [];
    return together;
  }

  /** Starting or ending a call forgets what was waiting. */
  reset(): void {
    this.phase = "listening";
    this.held = [];
  }
}
