import { notifySupport, type NotifySupport } from "@puppet-master/client-core/state/notify";

/* Per-kind tag prefixes keep a finish and a needs-input on the same session
   apart. Each alert also takes a sequence suffix, because a browser replaces
   a notification with a matching tag silently instead of announcing it. */
const NOTIFY_TAG_PREFIXES = {
  "needs-input": "pm-needs-input-",
  finished: "pm-finished-",
  approval: "pm-approval-",
} as const;

export type NotifyKind = keyof typeof NOTIFY_TAG_PREFIXES;
const BEEP_TONE_HZ = 880;
const BEEP_DURATION_MS = 130;
const BEEP_GAIN = 0.06;
const MS_PER_SECOND = 1_000;

/**
 * Browser-side effects for needs-input alerts: a Web Notification and
 * an optional synthesized beep. Permission is only ever requested
 * through requestPermission, never implicitly.
 */
export class Notifier {
  onActivate: ((sessionId: string) => void) | null = null;
  /** Opens one approval; an approval alert's id is the call, not a session. */
  onActivateApproval: ((approvalId: string) => void) | null = null;
  soundEnabled = false;
  private sequence = 0;
  private shown = new Map<string, Notification>();

  get support(): NotifySupport {
    return notifySupport(
      typeof Notification !== "undefined",
      typeof window !== "undefined" ? window.isSecureContext : undefined,
    );
  }

  get supported(): boolean {
    return this.support.supported;
  }

  get permission(): NotificationPermission {
    return this.supported ? Notification.permission : "denied";
  }

  async requestPermission(): Promise<NotificationPermission> {
    if (!this.supported) return "denied";
    return Notification.requestPermission();
  }

  /** `sessionId` is the approval id when `kind` is "approval". */
  fire(title: string, body: string | undefined, sessionId: string, kind: NotifyKind = "needs-input"): void {
    if (this.soundEnabled) this.beep();
    if (!this.supported || Notification.permission !== "granted") return;
    const key = NOTIFY_TAG_PREFIXES[kind] + sessionId;
    try {
      this.sequence += 1;
      const notification = new Notification(title, {
        ...(body !== undefined ? { body } : {}),
        tag: `${key}-${this.sequence}`,
      });
      this.shown.get(key)?.close();
      this.shown.set(key, notification);
      notification.onclick = () => {
        window.focus();
        if (kind === "approval") this.onActivateApproval?.(sessionId);
        else this.onActivate?.(sessionId);
        notification.close();
      };
      notification.onclose = () => {
        if (this.shown.get(key) === notification) this.shown.delete(key);
      };
    } catch {
      // Some browsers only allow notifications from a service worker; degrade to the beep.
    }
  }

  private beep(): void {
    type AudioCtor = typeof AudioContext;
    const Ctor: AudioCtor | undefined =
      typeof AudioContext !== "undefined"
        ? AudioContext
        : (globalThis as { webkitAudioContext?: AudioCtor }).webkitAudioContext;
    if (!Ctor) return;
    const ctx = new Ctor();
    const osc = ctx.createOscillator();
    const gain = ctx.createGain();
    osc.type = "sine";
    osc.frequency.value = BEEP_TONE_HZ;
    gain.gain.value = BEEP_GAIN;
    osc.connect(gain);
    gain.connect(ctx.destination);
    osc.start();
    osc.stop(ctx.currentTime + BEEP_DURATION_MS / MS_PER_SECOND);
    osc.onended = () => void ctx.close();
  }
}
