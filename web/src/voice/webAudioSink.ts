/**
 * Plays the daemon's rendered speech in a browser.
 *
 * Frames arrive as they are synthesized, so they are scheduled back to back
 * on one clock rather than played as they land: scheduling each one "now"
 * leaves audible gaps whenever the network hesitates.
 */

import type { AudioSink } from "@puppet-master/client-core/voice/controller";

/** Signed 16-bit little-endian, which is what the daemon sends by default. */
const PCM16 = "pcm_s16le";

const INT16_FULL_SCALE = 32768;

/** A little lead time, so the first frame is scheduled rather than late. */
const START_LEAD_SECONDS = 0.08;

export class WebAudioSink implements AudioSink {
  private context?: AudioContext;
  private codec = "";
  private sampleRate = 0;
  /** Where the next frame goes on the context's clock. */
  private nextStart = 0;
  private playing = new Set<AudioBufferSourceNode>();
  /** An odd trailing byte, since a frame can split a sample in half. */
  private partial?: Uint8Array;

  constructor(private onUnsupported?: (codec: string) => void) {}

  supports(codec: string): boolean {
    return codec === PCM16;
  }

  begin(codec: string, sampleRate: number): void {
    this.codec = codec;
    this.sampleRate = sampleRate;
    this.partial = undefined;
    if (codec !== PCM16) {
      // Anything else needs a decoder this does not carry. Saying so is
      // better than playing noise.
      this.onUnsupported?.(codec);
      return;
    }
    const context = this.ensureContext();
    // Resuming is a no-op unless autoplay policy suspended it, which it
    // does until the page has been interacted with.
    void context.resume();
    this.nextStart = Math.max(this.nextStart, context.currentTime + START_LEAD_SECONDS);
  }

  push(frame: Uint8Array): void {
    if (this.codec !== PCM16 || frame.length === 0) return;
    const context = this.ensureContext();
    const bytes = this.withPartial(frame);
    const samples = Math.floor(bytes.length / 2);
    if (samples === 0) return;
    const view = new DataView(bytes.buffer, bytes.byteOffset, samples * 2);
    const buffer = context.createBuffer(1, samples, this.sampleRate);
    const channel = buffer.getChannelData(0);
    for (let i = 0; i < samples; i += 1) {
      channel[i] = view.getInt16(i * 2, true) / INT16_FULL_SCALE;
    }
    const source = context.createBufferSource();
    source.buffer = buffer;
    source.connect(context.destination);
    const at = Math.max(this.nextStart, context.currentTime);
    source.start(at);
    this.nextStart = at + buffer.duration;
    this.playing.add(source);
    source.onended = () => this.playing.delete(source);
  }

  end(): void {
    this.partial = undefined;
  }

  stop(): void {
    for (const source of this.playing) {
      try {
        source.stop();
      } catch {
        // Already finished, which is the same outcome.
      }
    }
    this.playing.clear();
    this.partial = undefined;
    this.nextStart = this.context?.currentTime ?? 0;
  }

  /** Carries a split sample across frames rather than dropping a byte. */
  private withPartial(frame: Uint8Array): Uint8Array {
    const joined = this.partial
      ? (() => {
          const merged = new Uint8Array(this.partial.length + frame.length);
          merged.set(this.partial, 0);
          merged.set(frame, this.partial.length);
          return merged;
        })()
      : frame;
    const whole = joined.length - (joined.length % 2);
    this.partial = whole === joined.length ? undefined : joined.slice(whole);
    return joined.slice(0, whole);
  }

  private ensureContext(): AudioContext {
    this.context ??= new AudioContext();
    return this.context;
  }
}
