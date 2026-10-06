import type { TerminalOwnership, TerminalSize } from "./pty";

type ViewerState = "opening" | "resizing" | "claiming" | "owning" | "following" | "dismissed";

export class TerminalSizePrompt {
  private local: TerminalSize | null = null;
  private state: ViewerState = "opening";

  constructor(
    private measure: () => TerminalSize | null,
    private changed: (show: boolean) => void,
    private resize: (size: TerminalSize) => void,
  ) {}

  blocked(): boolean {
    return this.state === "following" || this.state === "dismissed";
  }

  needsClaim(): boolean {
    return this.state === "opening" || this.state === "resizing";
  }

  localChanged(): boolean {
    const next = this.measure();
    if (!next || (next.cols === this.local?.cols && next.rows === this.local.rows)) return false;
    this.local = next;
    this.state = "resizing";
    this.changed(false);
    return true;
  }

  requested(_size: TerminalSize): void {
    this.local = this.measure();
    this.state = "claiming";
    this.changed(false);
  }

  observe(ownership: TerminalOwnership): void {
    if (this.state === "resizing") return;
    if (ownership.local) {
      this.state = "owning";
      this.changed(false);
      return;
    }
    if (this.state === "opening") return;
    const dismissed = this.state === "dismissed";
    this.state = dismissed ? "dismissed" : "following";
    const fit = this.measure();
    this.changed(!dismissed && fit !== null && (ownership.cols !== fit.cols || ownership.rows !== fit.rows));
  }

  update(): void {
    const size = this.measure();
    if (!size) return;
    this.requested(size);
    this.resize(size);
  }

  dismiss(): void {
    this.state = "dismissed";
    this.changed(false);
  }

  reset(): void {
    this.local = this.measure();
    this.state = "opening";
    this.changed(false);
  }
}
