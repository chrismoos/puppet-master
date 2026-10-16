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

/**
 * How much of an utterance has to be sound the agent just made for it to
 * be the speaker leaking back into the microphone rather than the user.
 */
export const ECHO_OVERLAP = 0.6;

/**
 * Below this many words an utterance is judged by [`BARGE_IN_WORDS`]
 * alone. "Yes" is a word the agent may well have said too, and dropping
 * it would lose the answer to the question it just asked.
 */
export const ECHO_WORDS_MIN = 3;

/** How many characters a run has to share to count as the same sound. */
const ECHO_RUN = 3;

function words(text: string): number {
  return text.trim().split(/\s+/).filter(Boolean).length;
}

function flatten(text: string): string {
  return text
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, " ")
    .trim();
}

function runs(text: string): Set<string> {
  const found = new Set<string>();
  for (let i = 0; i + ECHO_RUN <= text.length; i += 1) {
    found.add(text.slice(i, i + ECHO_RUN));
  }
  return found;
}

/**
 * Whether an utterance is the agent's own speech coming back through the
 * microphone. On the speaker, echo cancellation leaves enough of a
 * sentence to pass for a whole turn, and acting on it asks the agent
 * about what it just said.
 *
 * Scored on short runs of characters rather than on whole words, because
 * recognition does not return speech verbatim: it mishears words, splits
 * them and joins them, and a comparison that wanted the words back as
 * written would call that a new question.
 */
export function looksLikeEcho(said: string, spoken: string): boolean {
  if (words(said) < ECHO_WORDS_MIN) return false;
  const heard = runs(flatten(said));
  const out = runs(flatten(spoken));
  if (heard.size === 0 || out.size === 0) return false;
  let shared = 0;
  for (const run of heard) {
    if (out.has(run)) shared += 1;
  }
  return shared / heard.size >= ECHO_OVERLAP;
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
  /** What the agent last read out, which the microphone may hear back. */
  private spoken = "";

  /** Records what is being read out, so hearing it back is not a turn. */
  spoke(text: string): void {
    const said = text.trim();
    if (said) this.spoken = said;
  }

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
    // Checked before the phase, because playback outlasts the answer the
    // daemon has already finished sending: the tail of it arrives while
    // the call is listening again, where nothing else would question it.
    if (looksLikeEcho(said, this.spoken)) return { kind: "ignored" };
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
    this.spoken = "";
  }
}
