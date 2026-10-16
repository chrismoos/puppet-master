/**
 * The phone's own microphone and speaker, behind the interfaces the
 * shared voice controller uses.
 *
 * Recognition runs on device and only its text is sent, so audio never
 * leaves the phone. Speech comes back down already rendered, because the
 * daemon holds the provider credential and one voice sounds the same on
 * every client.
 */

import { requireNativeModule } from "expo-modules-core";
import type { AudioSink } from "@puppet-master/client-core/voice/controller";

const LOG = "[voice]";

interface TranscriptEvent {
  text: string;
  /// Always false: on-device recognition finalizes a task whenever the
  /// speaker pauses, and the module starts another, so what ends an
  /// utterance is the user letting go of the button.
  isFinal: boolean;
}

interface VoiceNativeModule {
  dictationAvailable(): boolean;
  requestPermissions(): Promise<boolean>;
  startListening(inCall: boolean): Promise<void>;
  stopListening(): void;
  endUtterance(): void;
  prepareSpeech(): void;
  setOutputToSpeaker(on: boolean): void;
  audioRoute(): string;
  beginSpeech(codec: string, sampleRate: number): void;
  pushSpeech(frameBase64: string): void;
  endSpeech(): void;
  stopSpeech(): void;
  addListener(
    event: "onTranscript" | "onDictationError",
    listener: (payload: TranscriptEvent & { message?: string }) => void,
  ): { remove(): void };
}

let cached: VoiceNativeModule | null | undefined;

/**
 * The module, or null in a build without it. A missing module is not an
 * error: typing is the same interface, and Expo Go has no native code.
 */
export function voiceModule(): VoiceNativeModule | null {
  if (cached !== undefined) return cached;
  try {
    cached = requireNativeModule<VoiceNativeModule>("PuppetVoice");
  } catch {
    console.warn(
      `${LOG} PuppetVoice native module is missing, so talking and spoken ` +
        "answers are unavailable. Typing still works. Rebuild with the voice " +
        "config plugin applied.",
    );
    cached = null;
  }
  return cached ?? null;
}

/**
 * Readies the audio path before an answer arrives. Starting the audio
 * session and the engine is most of what made the first sound late, and
 * it can be paid for while the voice screen is opening instead.
 */
export function prepareSpeech(): void {
  try {
    voiceModule()?.prepareSpeech();
  } catch {
    // Nothing readied. The first answer starts it instead.
  }
}

/**
 * Sends the answer to the handset's own speaker, or hands the choice
 * back to the system, which is what picks headphones or a car. The
 * system resets this on a route change, so a call asks again per answer.
 */
export function setOutputToSpeaker(on: boolean): void {
  try {
    voiceModule()?.setOutputToSpeaker(on);
  } catch {
    // The route is the system's to give. Nothing to do about a refusal.
  }
}

/** Where the answer is going now, in a word: speaker, earpiece, headphones, car. */
export function audioRoute(): string {
  try {
    return voiceModule()?.audioRoute() ?? "none";
  } catch {
    return "none";
  }
}

export function dictationAvailable(): boolean {
  const native = voiceModule();
  if (!native) return false;
  try {
    return native.dictationAvailable();
  } catch {
    return false;
  }
}

/** Plays what the daemon rendered. */
export class NativeAudioSink implements AudioSink {
  constructor(private native: VoiceNativeModule | null = voiceModule()) {}

  /** The module decodes nothing: it plays the frames it is given. */
  supports(codec: string): boolean {
    return codec === "pcm_s16le";
  }



  begin(codec: string, sampleRate: number): void {
    this.native?.beginSpeech(codec, sampleRate);
  }

  push(frame: Uint8Array): void {
    // The bridge carries a string far more cheaply than a byte array.
    this.native?.pushSpeech(base64(frame));
  }

  end(): void {
    this.native?.endSpeech();
  }

  stop(): void {
    this.native?.stopSpeech();
  }
}

/**
 * Holds the microphone open until it is stopped, reporting the
 * transcript as it firms up.
 *
 * Push to talk rather than listening continuously: the user decides when
 * it hears them, and a button is something a test can press.
 */
export class Dictation {
  private subscriptions: Array<{ remove(): void }> = [];
  /// Counts holds, so one abandoned while the permission prompt is up
  /// does not leave the microphone open behind the next one.
  private generation = 0;

  constructor(private native: VoiceNativeModule | null = voiceModule()) {}

  /**
   * Asks for the microphone without listening, so the first hold is not
   * racing a permission dialog. Safe to call repeatedly: iOS answers
   * from its stored decision after the first time.
   */
  async permitted(): Promise<boolean> {
    const native = this.native;
    if (!native) return false;
    try {
      return await native.requestPermissions();
    } catch {
      return false;
    }
  }

  /**
   * Holds the microphone open. In a call the speaker is live at the same
   * time, so the native side keeps echo cancellation on for it.
   */
  async start(
    onTranscript: (text: string, isFinal: boolean) => void,
    onError: (message: string) => void,
    inCall = false,
  ): Promise<void> {
    const native = this.native;
    if (!native) {
      onError("this build cannot listen, so type instead");
      return;
    }
    this.generation += 1;
    const mine = this.generation;
    const allowed = await native.requestPermissions();
    if (!allowed) {
      onError("microphone or speech access was declined");
      return;
    }
    // Released while the prompt was up, so there is nothing to listen
    // for. Starting now would hold the microphone open with nobody
    // reading what it hears.
    if (mine !== this.generation) return;
    this.subscriptions.push(
      native.addListener("onTranscript", (event) =>
        onTranscript(event.text, event.isFinal),
      ),
      native.addListener("onDictationError", (event) =>
        onError(event.message ?? "dictation failed"),
      ),
    );
    try {
      await native.startListening(inCall);
    } catch (err) {
      onError(err instanceof Error ? err.message : String(err));
      this.stop();
      return;
    }
    if (mine !== this.generation) this.stop();
  }

  /**
   * Ends an utterance without letting go of the microphone, which is
   * what a call does. The transcript starts again from nothing, so
   * words already acted on are not heard a second time as the start of
   * whatever is said next.
   */
  nextUtterance(): void {
    this.native?.endUtterance();
  }

  stop(): void {
    this.generation += 1;
    this.native?.stopListening();
    for (const subscription of this.subscriptions) subscription.remove();
    this.subscriptions = [];
  }
}

const BASE64 = "ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

/**
 * Encodes without Buffer, which React Native does not carry, and without
 * btoa, which rejects bytes above 0xff when they arrive as a string.
 */
export function base64(bytes: Uint8Array): string {
  let out = "";
  for (let i = 0; i < bytes.length; i += 3) {
    const a = bytes[i];
    const b = bytes[i + 1];
    const c = bytes[i + 2];
    out += BASE64[a >> 2];
    out += BASE64[((a & 3) << 4) | ((b ?? 0) >> 4)];
    out += b === undefined ? "=" : BASE64[((b & 15) << 2) | ((c ?? 0) >> 6)];
    out += c === undefined ? "=" : BASE64[c & 63];
  }
  return out;
}
