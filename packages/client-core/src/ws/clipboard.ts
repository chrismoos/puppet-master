export interface ClipboardWriter {
  writeText(text: string): Promise<void>;
}

export interface NavigatorClipboardHost {
  clipboard?: ClipboardWriter;
}

export const CLIPBOARD_UNAVAILABLE_MESSAGE = "clipboard unavailable on this origin";

/** Browsers expose navigator.clipboard only to secure contexts (HTTPS or localhost). */
export const INSECURE_ORIGIN_HINT = "this page is not HTTPS or localhost, so copies need a click or keypress";

/**
 * The async clipboard API is missing, which is what a plain-HTTP origin
 * looks like. A gesture-backed legacy copy may still succeed, so callers
 * park the text rather than giving up.
 */
export class ClipboardUnavailableError extends Error {
  constructor() {
    super(CLIPBOARD_UNAVAILABLE_MESSAGE);
    this.name = "ClipboardUnavailableError";
  }
}

export function clipboardAvailable(host: NavigatorClipboardHost | undefined): boolean {
  return typeof host?.clipboard?.writeText === "function";
}

/**
 * Writes text through the async clipboard API, rejecting with
 * ClipboardUnavailableError instead of throwing a TypeError when the
 * browser exposes no clipboard.
 */
export function writeClipboardText(
  text: string,
  host: NavigatorClipboardHost | undefined = (globalThis as { navigator?: NavigatorClipboardHost }).navigator,
): Promise<void> {
  if (!clipboardAvailable(host)) return Promise.reject(new ClipboardUnavailableError());
  try {
    return host!.clipboard!.writeText(text);
  } catch (error) {
    return Promise.reject(error);
  }
}

export type ClipboardCopyState =
  | { kind: "idle" }
  | { kind: "copied"; chars: number }
  | { kind: "pending"; chars: number; insecureOrigin: boolean }
  | { kind: "failed"; chars: number; reason: string };

export function clipboardFailureReason(error: unknown): string {
  if (error instanceof Error) return error.message || error.name || "copy failed";
  return typeof error === "string" && error ? error : "copy failed";
}

/** A gesture-free write that has not settled by now is treated as refused. */
export const CLIPBOARD_SETTLE_TIMEOUT_MS = 1_500;

export interface DeferredClipboardOptions {
  settleTimeoutMs?: number;
  setTimeout?: (handler: () => void, ms: number) => unknown;
}

export interface CopyOptions {
  /** A gesture-backed copy, such as a selection, needs no success notice. */
  silentSuccess?: boolean;
}

/**
 * Holds a clipboard write the browser refused until a later gesture can
 * carry it. Writes driven by PTY output (OSC 52) have no gesture in their
 * call stack: Safari refuses them, Chrome refuses them while the document
 * is unfocused, an insecure origin has no async clipboard API at all, and
 * Chromium can leave the write pending forever behind a permission query
 * that never shows, so the first attempt is also bounded by a timeout.
 */
export class DeferredClipboardCopy {
  private pendingText: string | null = null;
  private state: ClipboardCopyState = { kind: "idle" };
  private generation = 0;
  private readonly settleTimeoutMs: number;
  private readonly schedule: (handler: () => void, ms: number) => unknown;

  constructor(
    private readonly write: (text: string) => Promise<void>,
    private readonly onChange: (state: ClipboardCopyState) => void = () => {},
    options: DeferredClipboardOptions = {},
  ) {
    this.settleTimeoutMs = options.settleTimeoutMs ?? CLIPBOARD_SETTLE_TIMEOUT_MS;
    this.schedule = options.setTimeout ?? ((handler, ms) => setTimeout(handler, ms));
  }

  current(): ClipboardCopyState {
    return this.state;
  }

  hasPending(): boolean {
    return this.pendingText !== null;
  }

  /** Attempts the write now; any refusal parks the text for the next gesture. */
  async copy(text: string, options: CopyOptions = {}): Promise<ClipboardCopyState> {
    const generation = ++this.generation;
    this.pendingText = null;
    try {
      await Promise.race([this.write(text), this.unsettled()]);
      if (generation !== this.generation) return this.state;
      return this.transition(options.silentSuccess ? { kind: "idle" } : { kind: "copied", chars: text.length });
    } catch (error) {
      if (generation !== this.generation) return this.state;
      this.pendingText = text;
      return this.transition({
        kind: "pending",
        chars: text.length,
        insecureOrigin: error instanceof ClipboardUnavailableError,
      });
    }
  }

  /**
   * Retries the parked write. Call it synchronously from a user gesture so
   * the browser sees the activation. Returns null when nothing is pending.
   */
  completeFromGesture(): Promise<ClipboardCopyState> | null {
    const text = this.pendingText;
    if (text === null) return null;
    this.pendingText = null;
    const generation = ++this.generation;
    return this.write(text).then(
      () => (generation === this.generation
        ? this.transition({ kind: "copied", chars: text.length })
        : this.state),
      (error: unknown) => (generation === this.generation
        ? this.transition({ kind: "failed", chars: text.length, reason: clipboardFailureReason(error) })
        : this.state),
    );
  }

  dismiss(): void {
    this.generation += 1;
    this.pendingText = null;
    this.transition({ kind: "idle" });
  }

  private unsettled(): Promise<never> {
    return new Promise((_, reject) => {
      this.schedule(() => {
        const error = new Error(`clipboard write did not settle within ${this.settleTimeoutMs}ms`);
        error.name = "TimeoutError";
        reject(error);
      }, this.settleTimeoutMs);
    });
  }

  private transition(state: ClipboardCopyState): ClipboardCopyState {
    this.state = state;
    this.onChange(state);
    return state;
  }
}
