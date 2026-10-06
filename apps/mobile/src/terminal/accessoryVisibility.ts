// Visibility state machine for the terminal accessory key row. The row is
// hidden by default and follows the software keyboard, where its keys are
// actually needed. A manual toggle overrides the keyboard-driven state until
// the keyboard next changes, so the row can be summoned without the keyboard
// and dismissed while typing.

export class AccessoryRowVisibility {
  private keyboardUp = false;
  private override: "shown" | "hidden" | null = null;

  visible(): boolean {
    if (this.override !== null) return this.override === "shown";
    return this.keyboardUp;
  }

  keyboardShown(): void {
    // iOS re-fires keyboard-show on frame changes mid-session; only a fresh
    // appearance resets a manual override.
    if (!this.keyboardUp) this.override = null;
    this.keyboardUp = true;
  }

  keyboardHidden(): void {
    this.keyboardUp = false;
    this.override = null;
  }

  toggle(): boolean {
    this.override = this.visible() ? "hidden" : "shown";
    return this.visible();
  }
}
