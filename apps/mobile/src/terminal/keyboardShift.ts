// Sequences the native container shift that keeps the terminal's bottom row
// visible while the software keyboard animates. The visible shift is a
// translate transform that tracks the keyboard animation, because a
// transform needs no layout pass, WebView refit, or PTY resize per frame.
//
// The layout inset (paddingBottom) is applied immediately when the keyboard
// geometry changes, so the container's resting size is always correct for the
// keyboard's target position. The transform is a visual offset that lets the
// content slide into that resting size over the keyboard's animation duration
// instead of snapping.
//
// On show: paddingBottom = overlap immediately, translateY = +overlap (content
// starts at its pre-keyboard position), animate translateY to 0 (content
// slides up to meet the keyboard).
//
// On hide: paddingBottom = 0 immediately, translateY = -prevOverlap (content
// stays at its keyboard-up position), animate translateY to 0 (content slides
// down to fill the restored space).
//
// This ordering means the inset is always authoritative and never depends on
// an animation callback to reach the correct resting state. If the animation
// is interrupted, the inset is already correct and only the visual slide is
// lost — the content jumps to its final position, which is correct.

export type KeyboardShiftDirective =
  /** Set the content translate immediately, without animating. */
  | { kind: "jump"; toPx: number }
  /** Animate the content translate, tracking the keyboard. */
  | { kind: "animate"; toPx: number; durationMs: number }
  /** Bottom inset the layout owns; applied immediately. */
  | { kind: "inset"; px: number };

export class KeyboardShiftPlanner {
  private insetPx = 0;

  willShow(overlapPx: number, durationMs: number): KeyboardShiftDirective[] {
    const overlap = Math.max(0, Math.round(overlapPx));
    const prevInset = this.insetPx;
    this.insetPx = overlap;

    const out: KeyboardShiftDirective[] = [{ kind: "inset", px: overlap }];

    if (!(durationMs > 0)) {
      out.push({ kind: "jump", toPx: 0 });
      return out;
    }

    // The content was visually at prevInset worth of shift. Moving the inset
    // to `overlap` changes the container size immediately; to keep the visual
    // position unchanged, offset by the difference, then animate to 0.
    const visualOffset = overlap - prevInset;
    out.push(
      { kind: "jump", toPx: visualOffset },
      { kind: "animate", toPx: 0, durationMs },
    );
    return out;
  }

  willHide(durationMs: number): KeyboardShiftDirective[] {
    const prevInset = this.insetPx;
    this.insetPx = 0;

    const out: KeyboardShiftDirective[] = [{ kind: "inset", px: 0 }];

    if (!(durationMs > 0)) {
      out.push({ kind: "jump", toPx: 0 });
      return out;
    }

    // The container just grew by prevInset (padding removed). To keep the
    // content visually in place, offset upward by prevInset, then animate
    // down to 0.
    out.push(
      { kind: "jump", toPx: -prevInset },
      { kind: "animate", toPx: 0, durationMs },
    );
    return out;
  }

  /** No-op retained for call-site compatibility. The resting state is already
   *  correct from the inset applied at event time, so there is nothing to
   *  release or settle. */
  animationEnded(): KeyboardShiftDirective[] {
    return [];
  }
}
