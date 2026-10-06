import type { PtyFrame } from "./pty";

export function terminalEvictionPriority(key: string): number {
  return key.startsWith("s:") ? 1 : 0;
}

export class BackgroundPtyBuffer {
  private frames: PtyFrame[] = [];
  /** Live output held since the last replay, which is what the cap bounds. */
  private liveBytes = 0;
  private replayBytes = 0;

  constructor(private capacity: number) {}

  push(frame: PtyFrame): boolean {
    if (frame.replay) {
      // A replay supersedes the backlog and is bounded by the server's
      // terminal model, so it is held regardless of the byte cap; capping
      // it would reject state snapshots and loop the resync forever.
      this.frames = [frame];
      this.replayBytes = frame.data.byteLength;
      this.liveBytes = 0;
      return true;
    }
    // A held replay is deliberately allowed to exceed the cap, so counting
    // it here would overflow on the next live frame however small, and the
    // resync that answers brings another replay just as large. The cap is
    // what a hidden layer may accumulate before its backlog stops being
    // worth keeping, so it measures live output only.
    if (this.liveBytes + frame.data.byteLength > this.capacity) {
      this.frames = [];
      this.liveBytes = 0;
      this.replayBytes = 0;
      return false;
    }
    this.frames.push(frame);
    this.liveBytes += frame.data.byteLength;
    return true;
  }

  drain(): PtyFrame[] {
    const frames = this.frames;
    this.frames = [];
    this.liveBytes = 0;
    this.replayBytes = 0;
    return frames;
  }

  bytesHeld(): number {
    return this.replayBytes + this.liveBytes;
  }
}
