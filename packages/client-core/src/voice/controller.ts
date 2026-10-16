/**
 * One voice conversation, as the UI sees it.
 *
 * The reasoning, the tools and the speech all happen in the daemon, so this
 * sends text up and plays what comes back. Audio playback is injected rather
 * than built in: a browser and a phone have different audio stacks, and the
 * logic around them is the same.
 */

import type { VoiceDelta } from "../gen/pm/v1/pm_pb";

/** Where rendered speech goes. */
export interface AudioSink {
  /** A new stretch of speech is coming, in this codec. */
  begin(codec: string, sampleRate: number): void;
  /** One frame, in the codec the last `begin` named. */
  push(frame: Uint8Array): void;
  /** No more frames for this stretch. */
  end(): void;
  /** Abandon anything queued or playing, now. */
  stop(): void;
  /**
   * Whether this sink can play a codec. A sink that cannot say is
   * assumed able, so an older one keeps working.
   */
  supports?(codec: string): boolean;
}

/** A sink for a client that cannot play audio, or has it turned off. */
export const silentSink: AudioSink = {
  begin: () => {},
  push: () => {},
  end: () => {},
  stop: () => {},
};

export type VoiceStatus = "idle" | "thinking" | "speaking";

export interface VoiceLine {
  from: "user" | "agent";
  text: string;
}

export interface VoiceUiState {
  status: VoiceStatus;
  /** The exchange so far, oldest first. */
  lines: readonly VoiceLine[];
  /** Why the last turn stopped, when it stopped badly. */
  error: string;
}

/** What the controller needs from the client, so tests can supply it. */
export interface VoiceTransport {
  voiceTurn(
    text: string,
    options: { conversationId?: bigint; wantsAudio?: boolean },
  ): Promise<bigint | undefined>;
  voiceCancel(conversationId: bigint): Promise<unknown>;
  onVoiceDelta(listener: (delta: VoiceDelta) => void): () => void;
}

export class VoiceController {
  private status: VoiceStatus = "idle";
  private lines: VoiceLine[] = [];
  private error = "";
  private conversation?: bigint;
  private listeners = new Set<() => void>();
  private unsubscribe?: () => void;
  /** Whether the sink has an open stretch that still needs ending. */
  private speaking = false;
  /** Whether the codec in flight is one the sink said it can play. */
  private playable = true;

  constructor(
    private transport: VoiceTransport,
    private sink: AudioSink = silentSink,
  ) {}

  /** Starts listening for answers. Call once, and dispose when done. */
  start(): void {
    this.unsubscribe ??= this.transport.onVoiceDelta((delta) => this.apply(delta));
  }

  dispose(): void {
    this.unsubscribe?.();
    this.unsubscribe = undefined;
    this.sink.stop();
  }

  subscribe(listener: () => void): () => void {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  }

  state(): VoiceUiState {
    return { status: this.status, lines: this.lines, error: this.error };
  }

  /**
   * Sends what the user said. A turn while one is already running replaces
   * it, which is what interrupting the agent means.
   */
  async say(text: string, wantsAudio: boolean): Promise<void> {
    const said = text.trim();
    if (!said) return;
    // Speech already handed to the sink plays on after the daemon has
    // finished sending it, so the status is idle while the last answer
    // is still coming out. A new turn silences it either way.
    this.sink.stop();
    if (this.status !== "idle") await this.cancel();
    this.error = "";
    this.lines = [...this.lines, { from: "user", text: said }];
    this.status = "thinking";
    this.changed();
    try {
      const conversation = await this.transport.voiceTurn(said, {
        conversationId: this.conversation,
        wantsAudio,
      });
      // A reconnect loses the conversation, and continuing one the daemon
      // has forgotten would be answered without any of its history.
      if (conversation !== undefined) this.conversation = conversation;
    } catch (err) {
      this.status = "idle";
      this.error = err instanceof Error ? err.message : String(err);
      this.changed();
    }
  }

  /** Stops the answer in flight, including speech still being rendered. */
  async cancel(): Promise<void> {
    this.sink.stop();
    this.speaking = false;
    const conversation = this.conversation;
    this.status = "idle";
    this.changed();
    if (conversation !== undefined) {
      try {
        await this.transport.voiceCancel(conversation);
      } catch {
        // The turn is already abandoned locally. A failed cancel means
        // the daemon finishes an answer nobody is listening to.
      }
    }
  }

  private apply(delta: VoiceDelta): void {
    if (this.conversation !== undefined && delta.conversationId !== this.conversation) {
      // Another client's conversation on a shared connection.
      return;
    }
    if (delta.error) {
      this.error = delta.error;
    }
    if (delta.text) {
      this.appendAgentText(delta.text);
    }
    if (delta.audioCodec) {
      // A provider configured to encode something this device cannot
      // play would otherwise be silence with nothing said about it.
      if (this.sink.supports?.(delta.audioCodec) === false) {
        this.error = `this device cannot play ${delta.audioCodec} audio, so the answer is text only`;
        this.playable = false;
      } else {
        this.playable = true;
        this.sink.begin(delta.audioCodec, delta.audioSampleRate);
        this.speaking = true;
        this.status = "speaking";
      }
    }
    if (delta.audio.length > 0 && this.playable) {
      this.sink.push(delta.audio);
    }
    if (delta.done) {
      if (this.speaking) {
        this.sink.end();
        this.speaking = false;
      }
      this.status = "idle";
    }
    this.changed();
  }

  /**
   * Text arrives in pieces, and consecutive pieces are one thing the agent
   * said. Keeping them as one line is what makes the transcript readable.
   */
  private appendAgentText(text: string): void {
    const last = this.lines[this.lines.length - 1];
    if (last?.from === "agent") {
      const joined = `${last.text} ${text}`.trim();
      this.lines = [...this.lines.slice(0, -1), { from: "agent", text: joined }];
      return;
    }
    this.lines = [...this.lines, { from: "agent", text }];
  }

  private changed(): void {
    for (const listener of this.listeners) listener();
  }
}
