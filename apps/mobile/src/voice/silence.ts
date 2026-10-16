/**
 * Gives up listening when the speaker stops.
 *
 * The countdown is to silence, not to a deadline, and the caller sets
 * how long it waits each time it restarts: a long wait for speech that
 * has not started, a short one to decide that a sentence has ended.
 */
export class SilenceTimer {
  private timer: ReturnType<typeof setTimeout> | null = null;

  constructor(private readonly onQuiet: () => void) {}

  /** Starts the countdown, abandoning any already running. */
  restart(afterMs: number): void {
    this.cancel();
    this.timer = setTimeout(() => {
      this.timer = null;
      this.onQuiet();
    }, afterMs);
  }

  cancel(): void {
    if (this.timer !== null) clearTimeout(this.timer);
    this.timer = null;
  }

  /** Whether a countdown is running, which is what listening means. */
  get waiting(): boolean {
    return this.timer !== null;
  }
}
