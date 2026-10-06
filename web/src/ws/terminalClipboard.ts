import {
  DeferredClipboardCopy,
  INSECURE_ORIGIN_HINT,
  type ClipboardCopyState,
} from "@puppet-master/client-core/ws/clipboard";
import { copyTextToClipboard } from "../clipboard";

const COPIED_NOTICE_MS = 2_000;
const FAILED_NOTICE_MS = 8_000;
/** Events that grant transient user activation in every supported browser. */
const GESTURE_EVENTS = ["pointerdown", "keydown"] as const;

export interface ClipboardStatusElement {
  className: string;
  hidden: boolean;
  textContent: string | null;
  appendChild(child: ClipboardStatusElement): unknown;
  addEventListener(type: string, listener: (event: unknown) => void): void;
  remove(): void;
}

export interface ClipboardButtonElement extends ClipboardStatusElement {
  type: string;
}

export interface ClipboardDocument {
  createElement(tag: "div" | "span"): ClipboardStatusElement;
  createElement(tag: "button"): ClipboardButtonElement;
  addEventListener(type: string, listener: (event: unknown) => void, options?: { capture: boolean }): void;
  removeEventListener(type: string, listener: (event: unknown) => void, options?: { capture: boolean }): void;
}

export interface ClipboardTimers {
  setTimeout(handler: () => void, ms: number): unknown;
  clearTimeout(handle: unknown): void;
}

export function clipboardStatusText(state: ClipboardCopyState): string {
  switch (state.kind) {
    case "idle":
      return "";
    case "copied":
      return `Copied ${state.chars} chars`;
    case "pending":
      return state.insecureOrigin
        ? `Copy of ${state.chars} chars is waiting for a click or keypress (${INSECURE_ORIGIN_HINT})`
        : `Copy of ${state.chars} chars is waiting for a click or keypress`;
    case "failed":
      return `Copy failed: ${state.reason}`;
  }
}

/**
 * Routes terminal copies through the clipboard and shows their outcome in
 * a small status element. A write the browser refuses for lack of user
 * activation is held and retried inside the next gesture on the document.
 */
export class TerminalClipboard {
  readonly el: ClipboardStatusElement;
  private readonly label: ClipboardStatusElement;
  private readonly button: ClipboardButtonElement;
  private readonly copies: DeferredClipboardCopy;
  private readonly onGesture = () => this.completeFromGesture();
  private attached = false;
  private hideTimer: unknown = null;

  constructor(
    private readonly doc: ClipboardDocument,
    write: (text: string) => Promise<void> = (text) => copyTextToClipboard(text),
    private readonly timers: ClipboardTimers = globalThis,
  ) {
    this.el = doc.createElement("div");
    this.el.className = "terminal-clipboard-status";
    this.el.hidden = true;
    this.label = doc.createElement("span");
    this.button = doc.createElement("button");
    this.button.type = "button";
    this.button.textContent = "copy now";
    this.button.hidden = true;
    this.button.addEventListener("click", () => this.completeFromGesture());
    this.el.appendChild(this.label);
    this.el.appendChild(this.button);
    this.copies = new DeferredClipboardCopy(write, (state) => this.render(state));
  }

  current(): ClipboardCopyState {
    return this.copies.current();
  }

  /** Starts completing parked copies on document gestures. */
  attach(): void {
    if (this.attached) return;
    this.attached = true;
    for (const type of GESTURE_EVENTS) this.doc.addEventListener(type, this.onGesture, { capture: true });
  }

  detach(): void {
    if (!this.attached) return;
    this.attached = false;
    for (const type of GESTURE_EVENTS) this.doc.removeEventListener(type, this.onGesture, { capture: true });
  }

  /** An OSC 52 copy from the application: no gesture backs it. */
  copyFromApplication(text: string): Promise<ClipboardCopyState> {
    return this.copies.copy(text);
  }

  /** A selection copy from a mouse or key event: success needs no notice. */
  copySelection(text: string): Promise<ClipboardCopyState> {
    return this.copies.copy(text, { silentSuccess: true });
  }

  dispose(): void {
    this.detach();
    this.clearHideTimer();
    this.el.remove();
  }

  private completeFromGesture(): void {
    void this.copies.completeFromGesture();
  }

  private render(state: ClipboardCopyState): void {
    this.clearHideTimer();
    this.el.className = `terminal-clipboard-status is-${state.kind}`;
    this.el.hidden = state.kind === "idle";
    this.label.textContent = clipboardStatusText(state);
    this.button.hidden = state.kind !== "pending";
    if (state.kind === "copied") this.hideAfter(COPIED_NOTICE_MS);
    else if (state.kind === "failed") this.hideAfter(FAILED_NOTICE_MS);
  }

  private hideAfter(ms: number): void {
    this.hideTimer = this.timers.setTimeout(() => {
      this.hideTimer = null;
      this.copies.dismiss();
    }, ms);
  }

  private clearHideTimer(): void {
    if (this.hideTimer === null) return;
    this.timers.clearTimeout(this.hideTimer);
    this.hideTimer = null;
  }
}
