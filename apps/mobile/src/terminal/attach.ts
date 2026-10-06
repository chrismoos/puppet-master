// Native-side ticket plumbing for the WebView terminal: every WebSocket
// attach needs a freshly minted one-use ticket, so the controller mints
// before the first init and re-mints (with backoff) whenever the WebView
// falls back to reconnecting or the terminal's generation moves on.

const REMINT_MIN_DELAY_MS = 500;
const REMINT_MAX_DELAY_MS = 8_000;
const REMINT_BACKOFF_FACTOR = 2;

export type AttachPhase =
  | { kind: "idle" }
  | { kind: "minting" }
  | { kind: "initSent"; generation: string }
  | { kind: "error"; reason: string };

export interface AttachHooks {
  /** Mints a one-use attach ticket for the terminal at this generation. */
  mintTicket(generation: string): Promise<string>;
  /** Sends the WebView init carrying the ticket. */
  sendInit(ticket: string, generation: string): void;
  onPhase(phase: AttachPhase): void;
}

export class TerminalAttachController {
  private ready = false;
  private minting = false;
  private disposed = false;
  private remintTimer: ReturnType<typeof setTimeout> | null = null;
  private remintDelayMs = REMINT_MIN_DELAY_MS;

  constructor(
    private hooks: AttachHooks,
    private generation: string,
  ) {}

  /** The WebView reported ready; mint the first ticket and init. */
  viewReady(): void {
    this.ready = true;
    this.resetBackoff();
    this.mintAndInit();
  }

  /** The terminal restarted under a new generation: the outstanding
   * ticket is bound to the old one, so mint fresh immediately. */
  setGeneration(generation: string): void {
    if (generation === this.generation) return;
    this.generation = generation;
    if (!this.ready) return;
    this.cancelRemint();
    this.resetBackoff();
    this.mintAndInit();
  }

  /** WebView terminal status stream; a reconnecting socket cannot
   * succeed on its burned ticket, so schedule a fresh mint + init. */
  viewStatus(phase: "connecting" | "replaying" | "online" | "reconnecting" | "closed"): void {
    if (!this.ready) return;
    if (phase === "online") {
      this.resetBackoff();
      return;
    }
    if (phase === "reconnecting") this.scheduleRemint();
  }

  /** The terminal socket was rejected as unauthenticated (burned or
   * expired ticket); mint a fresh one. */
  authRejected(): void {
    if (this.ready) this.scheduleRemint();
  }

  /** Manual retry after a visible mint failure. */
  retry(): void {
    if (!this.ready) return;
    this.cancelRemint();
    this.resetBackoff();
    this.mintAndInit();
  }

  dispose(): void {
    this.disposed = true;
    this.cancelRemint();
  }

  private scheduleRemint(): void {
    if (this.disposed || this.minting || this.remintTimer !== null) return;
    const delay = this.remintDelayMs;
    this.remintDelayMs = Math.min(this.remintDelayMs * REMINT_BACKOFF_FACTOR, REMINT_MAX_DELAY_MS);
    this.remintTimer = setTimeout(() => {
      this.remintTimer = null;
      this.mintAndInit();
    }, delay);
  }

  private mintAndInit(): void {
    if (this.disposed || this.minting) return;
    this.minting = true;
    const generation = this.generation;
    this.hooks.onPhase({ kind: "minting" });
    this.hooks
      .mintTicket(generation)
      .then((ticket) => {
        this.minting = false;
        if (this.disposed) return;
        if (generation !== this.generation) {
          // The terminal restarted mid-mint; this ticket is bound to a
          // dead generation and must not be presented.
          this.mintAndInit();
          return;
        }
        this.hooks.sendInit(ticket, generation);
        this.hooks.onPhase({ kind: "initSent", generation });
      })
      .catch((err: unknown) => {
        this.minting = false;
        if (this.disposed) return;
        if (generation !== this.generation) {
          // The generation moved while this mint was in flight; retry
          // with the current one instead of surfacing a stale error.
          this.mintAndInit();
          return;
        }
        this.hooks.onPhase({
          kind: "error",
          reason: err instanceof Error ? err.message : String(err),
        });
      });
  }

  private cancelRemint(): void {
    if (this.remintTimer === null) return;
    clearTimeout(this.remintTimer);
    this.remintTimer = null;
  }

  private resetBackoff(): void {
    this.remintDelayMs = REMINT_MIN_DELAY_MS;
  }
}
