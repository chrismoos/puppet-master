import { describe, expect, it } from "vitest";

import { KeyboardShiftPlanner, type KeyboardShiftDirective } from "./keyboardShift";

const OVERLAP = 300;
const DURATION = 250;

function kinds(directives: KeyboardShiftDirective[]): string[] {
  return directives.map((directive) => directive.kind);
}

/** Apply directives to a mutable state, returning it for chaining. */
function apply(
  directives: KeyboardShiftDirective[],
  state: { insetPx: number; translateY: number },
): { insetPx: number; translateY: number } {
  for (const d of directives) {
    if (d.kind === "inset") state.insetPx = d.px;
    if (d.kind === "jump") state.translateY = d.toPx;
    if (d.kind === "animate") state.translateY = d.toPx;
  }
  return state;
}

describe("KeyboardShiftPlanner", () => {
  it("shows by applying the inset immediately and animating the offset to 0", () => {
    const planner = new KeyboardShiftPlanner();
    const show = planner.willShow(OVERLAP, DURATION);
    expect(show).toEqual([
      { kind: "inset", px: OVERLAP },
      { kind: "jump", toPx: OVERLAP },
      { kind: "animate", toPx: 0, durationMs: DURATION },
    ]);
    expect(planner.animationEnded()).toEqual([]);
  });

  it("hides by removing the inset immediately and animating the offset to 0", () => {
    const planner = new KeyboardShiftPlanner();
    planner.willShow(OVERLAP, DURATION);
    planner.animationEnded();
    const hide = planner.willHide(DURATION);
    expect(hide).toEqual([
      { kind: "inset", px: 0 },
      { kind: "jump", toPx: -OVERLAP },
      { kind: "animate", toPx: 0, durationMs: DURATION },
    ]);
    expect(planner.animationEnded()).toEqual([]);
  });

  it("applies the inset immediately when the keyboard reports no animation", () => {
    const planner = new KeyboardShiftPlanner();
    expect(planner.willShow(OVERLAP, 0)).toEqual([
      { kind: "inset", px: OVERLAP },
      { kind: "jump", toPx: 0 },
    ]);
    expect(planner.animationEnded()).toEqual([]);
  });

  it("hides immediately when the keyboard reports no animation", () => {
    const planner = new KeyboardShiftPlanner();
    planner.willShow(OVERLAP, 0);
    expect(planner.willHide(0)).toEqual([
      { kind: "inset", px: 0 },
      { kind: "jump", toPx: 0 },
    ]);
  });

  it("retargets a show interrupted by hide", () => {
    const planner = new KeyboardShiftPlanner();
    planner.willShow(OVERLAP, DURATION);
    const hide = planner.willHide(DURATION);
    expect(hide).toEqual([
      { kind: "inset", px: 0 },
      { kind: "jump", toPx: -OVERLAP },
      { kind: "animate", toPx: 0, durationMs: DURATION },
    ]);
    expect(planner.animationEnded()).toEqual([]);
  });

  it("re-plans a growing keyboard from the correct visual offset", () => {
    const planner = new KeyboardShiftPlanner();
    planner.willShow(OVERLAP, DURATION);
    planner.animationEnded();
    const taller = OVERLAP + 36;
    const change = planner.willShow(taller, DURATION);
    expect(change).toEqual([
      { kind: "inset", px: taller },
      { kind: "jump", toPx: 36 },
      { kind: "animate", toPx: 0, durationMs: DURATION },
    ]);
  });

  it("re-plans a shrinking keyboard with a negative visual offset", () => {
    const planner = new KeyboardShiftPlanner();
    planner.willShow(OVERLAP, DURATION);
    planner.animationEnded();
    const shorter = OVERLAP - 36;
    const change = planner.willShow(shorter, DURATION);
    // Offset is shorter - prevInset = 264 - 300 = -36. The content starts
    // at its current position (old keyboard top) and slides down to the new
    // keyboard top. The -36 gap is behind the keyboard as it descends.
    expect(change).toEqual([
      { kind: "inset", px: shorter },
      { kind: "jump", toPx: -36 },
      { kind: "animate", toPx: 0, durationMs: DURATION },
    ]);
  });

  it("emits exactly one inset per show, at the start", () => {
    const planner = new KeyboardShiftPlanner();
    const all = [...planner.willShow(OVERLAP, DURATION), ...planner.animationEnded()];
    const insets = all.filter((d) => d.kind === "inset");
    expect(insets).toHaveLength(1);
    expect(all[0].kind).toBe("inset");
  });

  it("ignores a stale animation end after the state already settled", () => {
    const planner = new KeyboardShiftPlanner();
    planner.willShow(OVERLAP, DURATION);
    planner.animationEnded();
    expect(planner.animationEnded()).toEqual([]);
  });

  it("clamps a negative overlap to zero", () => {
    const planner = new KeyboardShiftPlanner();
    expect(planner.willShow(-40, DURATION)).toEqual([
      { kind: "inset", px: 0 },
      { kind: "jump", toPx: 0 },
      { kind: "animate", toPx: 0, durationMs: DURATION },
    ]);
  });
});

// ---------------------------------------------------------------------------
// Invariant: every animate directive targets 0
// ---------------------------------------------------------------------------

describe("KeyboardShiftPlanner — all animations target 0", () => {
  // In the inverted design every animate directive targets translateY = 0.
  // An RN Animated.timing is only cancelled by another timing on the same
  // Animated.Value, and that replacement also targets 0. So no matter how
  // many callbacks are dropped, the last animation in flight always heads
  // to 0 and the resting transform is 0. This is the real reason dropped
  // settles are harmless: the animation's target IS the correct resting
  // transform, and the inset was already set at event time.

  function collectAnimateTargets(directives: KeyboardShiftDirective[]): number[] {
    return directives
      .filter((d): d is { kind: "animate"; toPx: number; durationMs: number } => d.kind === "animate")
      .map((d) => d.toPx);
  }

  it("show targets 0", () => {
    const p = new KeyboardShiftPlanner();
    expect(collectAnimateTargets(p.willShow(OVERLAP, DURATION))).toEqual([0]);
  });

  it("hide targets 0", () => {
    const p = new KeyboardShiftPlanner();
    apply(p.willShow(OVERLAP, DURATION), { insetPx: 0, translateY: 0 });
    p.animationEnded();
    expect(collectAnimateTargets(p.willHide(DURATION))).toEqual([0]);
  });

  it("growing keyboard targets 0", () => {
    const p = new KeyboardShiftPlanner();
    apply(p.willShow(OVERLAP, DURATION), { insetPx: 0, translateY: 0 });
    p.animationEnded();
    expect(collectAnimateTargets(p.willShow(OVERLAP + 36, DURATION))).toEqual([0]);
  });

  it("shrinking keyboard targets 0", () => {
    const p = new KeyboardShiftPlanner();
    apply(p.willShow(OVERLAP, DURATION), { insetPx: 0, translateY: 0 });
    p.animationEnded();
    expect(collectAnimateTargets(p.willShow(OVERLAP - 36, DURATION))).toEqual([0]);
  });

  it("adversarial rapid sequence: every animate targets 0 and resting inset is correct", () => {
    const p = new KeyboardShiftPlanner();
    const allTargets: number[] = [];
    const state = { insetPx: 0, translateY: 0 };

    // Show
    const d1 = p.willShow(OVERLAP, DURATION);
    allTargets.push(...collectAnimateTargets(d1));
    apply(d1, state);

    // Hide mid-animation (no animationEnded)
    const d2 = p.willHide(DURATION);
    allTargets.push(...collectAnimateTargets(d2));
    apply(d2, state);

    // Show again mid-animation (no animationEnded)
    const d3 = p.willShow(OVERLAP, DURATION);
    allTargets.push(...collectAnimateTargets(d3));
    apply(d3, state);

    // Height change mid-animation (no animationEnded)
    const d4 = p.willShow(OVERLAP + 50, DURATION);
    allTargets.push(...collectAnimateTargets(d4));
    apply(d4, state);

    // Shrink mid-animation (no animationEnded)
    const d5 = p.willShow(OVERLAP - 20, DURATION);
    allTargets.push(...collectAnimateTargets(d5));
    apply(d5, state);

    // Hide (no animationEnded)
    const d6 = p.willHide(DURATION);
    allTargets.push(...collectAnimateTargets(d6));
    apply(d6, state);

    // Every single animate targeted 0
    expect(allTargets).toEqual([0, 0, 0, 0, 0, 0]);

    // Resting state: inset is authoritative from the last event, translateY
    // reaches 0 when the last animation completes (regardless of callbacks)
    expect(state.insetPx).toBe(0); // last event was hide
    expect(state.translateY).toBe(0); // animate target
  });
});

// ---------------------------------------------------------------------------
// Invariant: no gap during keyboard show (appearing or growing)
// ---------------------------------------------------------------------------

describe("KeyboardShiftPlanner — no-gap invariant", () => {
  /**
   * Checks that translateY is never negative after applying directives.
   *
   * gap = clipHeight - contentBottom = clipHeight - (clipHeight + translateY) = -translateY.
   * So gap > 0 iff translateY < 0.
   *
   * This helper is only meaningful for the SHOW path (keyboard appearing or
   * growing). During hide and keyboard-shrink, a negative translateY is
   * expected and harmless: the content bottom sits at the old keyboard top,
   * and the keyboard itself covers the exposed region as both descend
   * together. The helper cannot verify that invariant (it does not know the
   * keyboard position), so hide/shrink paths are tested separately for
   * correct resting state rather than per-directive gap absence.
   */
  function assertNoGap(
    directives: KeyboardShiftDirective[],
    state: { insetPx: number; translateY: number },
    label: string,
  ) {
    for (const d of directives) {
      if (d.kind === "inset") state.insetPx = d.px;
      if (d.kind === "jump") state.translateY = d.toPx;
      if (d.kind === "animate") state.translateY = d.toPx;
      if (state.translateY < 0) {
        throw new Error(
          `Gap of ${-state.translateY}px in "${label}" after ${d.kind}: translateY=${state.translateY}`,
        );
      }
    }
  }

  it("initial show produces no gap", () => {
    const p = new KeyboardShiftPlanner();
    const state = { insetPx: 0, translateY: 0 };
    assertNoGap(p.willShow(OVERLAP, DURATION), state, "willShow");
    assertNoGap(p.animationEnded(), state, "animationEnded");
  });

  it("growing keyboard produces no gap", () => {
    const p = new KeyboardShiftPlanner();
    const state = { insetPx: 0, translateY: 0 };
    apply(p.willShow(OVERLAP, DURATION), state);
    p.animationEnded();
    assertNoGap(p.willShow(OVERLAP + 36, DURATION), state, "willShow(grow)");
    assertNoGap(p.animationEnded(), state, "grow settled");
  });

  it("rapid show-hide-show with dropped settles produces no gap on show", () => {
    const p = new KeyboardShiftPlanner();
    const state = { insetPx: 0, translateY: 0 };

    assertNoGap(p.willShow(OVERLAP, DURATION), state, "show1");
    // Hide interrupts (no animationEnded)
    apply(p.willHide(DURATION), state);
    // Show again (no animationEnded)
    assertNoGap(p.willShow(OVERLAP, DURATION), state, "show2");
    assertNoGap(p.animationEnded(), state, "settled2");
    expect(state.insetPx).toBe(OVERLAP);
    expect(state.translateY).toBe(0);
  });

  it("shrinking keyboard has correct resting state (gap is behind keyboard)", () => {
    const p = new KeyboardShiftPlanner();
    const state = { insetPx: 0, translateY: 0 };
    apply(p.willShow(OVERLAP, DURATION), state);
    p.animationEnded();

    const shorter = OVERLAP - 36;
    const shrink = p.willShow(shorter, DURATION);
    apply(shrink, state);

    // During the animation the content bottom sits at the old keyboard top
    // and both descend together — the gap is behind the keyboard. The
    // resting state after the animation completes must be correct:
    expect(state.insetPx).toBe(shorter);
    expect(state.translateY).toBe(0);
  });

  it("hide has correct resting state (gap is behind keyboard during animation)", () => {
    // During hide, the keyboard slides down and the content follows.
    // The content bottom starts at the old keyboard top (negative translateY)
    // and both descend together, so the exposed region is always behind the
    // keyboard. The resting state after the animation must be correct.
    const p = new KeyboardShiftPlanner();
    const state = { insetPx: 0, translateY: 0 };
    apply([...p.willShow(OVERLAP, DURATION), ...p.animationEnded()], state);

    apply(p.willHide(DURATION), state);
    p.animationEnded();
    expect(state.insetPx).toBe(0);
    expect(state.translateY).toBe(0);
  });
});
