import { describe, expect, it } from "vitest";
import {
  reduceKeyboardInset,
  initialKeyboardInsetState,
  type KeyboardInsetState,
} from "./keyboardInset";

const base: KeyboardInsetState = { inset: 0, viewBottomY: 852 };

describe("reduceKeyboardInset", () => {
  it("show computes overlap from viewBottomY", () => {
    const r = reduceKeyboardInset(base, { type: "show", keyboardTopY: 552, duration: 250 });
    expect(r.toValue).toBe(300);
    expect(r.state.inset).toBe(300);
    expect(r.hard).toBe(false);
  });

  it("willHide resets to zero via animation", () => {
    const after = reduceKeyboardInset(base, { type: "show", keyboardTopY: 552, duration: 250 });
    const r = reduceKeyboardInset(after.state, { type: "willHide", duration: 250 });
    expect(r.toValue).toBe(0);
    expect(r.state.inset).toBe(0);
    expect(r.hard).toBe(false);
  });

  it("didHide forces exact zero even without preceding willHide", () => {
    const after = reduceKeyboardInset(base, { type: "show", keyboardTopY: 552, duration: 250 });
    expect(after.state.inset).toBe(300);
    const r = reduceKeyboardInset(after.state, { type: "didHide" });
    expect(r.toValue).toBe(0);
    expect(r.state.inset).toBe(0);
    expect(r.hard).toBe(true);
  });

  it("willHide then didHide both reach zero", () => {
    let s = reduceKeyboardInset(base, { type: "show", keyboardTopY: 552, duration: 250 }).state;
    const wh = reduceKeyboardInset(s, { type: "willHide", duration: 250 });
    expect(wh.toValue).toBe(0);
    s = wh.state;
    const dh = reduceKeyboardInset(s, { type: "didHide" });
    expect(dh.toValue).toBe(0);
    expect(dh.hard).toBe(true);
  });

  it("disable forces hard zero from any inset", () => {
    const after = reduceKeyboardInset(base, { type: "show", keyboardTopY: 400, duration: 250 });
    expect(after.state.inset).toBe(452);
    const r = reduceKeyboardInset(after.state, { type: "disable" });
    expect(r.toValue).toBe(0);
    expect(r.hard).toBe(true);
  });

  it("changeFrame with keyboard at screen bottom is zero", () => {
    const after = reduceKeyboardInset(base, { type: "show", keyboardTopY: 552, duration: 250 });
    const r = reduceKeyboardInset(after.state, {
      type: "changeFrame", keyboardTopY: 852, screenHeight: 852, duration: 250,
    });
    expect(r.toValue).toBe(0);
    expect(r.hard).toBe(false);
  });

  it("changeFrame with keyboard partially visible computes overlap", () => {
    const r = reduceKeyboardInset(base, {
      type: "changeFrame", keyboardTopY: 600, screenHeight: 852, duration: 250,
    });
    expect(r.toValue).toBe(252);
  });

  it("show with keyboard below view produces zero overlap", () => {
    const r = reduceKeyboardInset(base, { type: "show", keyboardTopY: 900, duration: 250 });
    expect(r.toValue).toBe(0);
  });

  it("full show then didHide cycle returns exact resting state, repeated", () => {
    let s: KeyboardInsetState = { ...base };
    for (let i = 0; i < 2; i++) {
      const show = reduceKeyboardInset(s, { type: "show", keyboardTopY: 552, duration: 250 });
      expect(show.toValue).toBe(300);
      s = show.state;
      const hide = reduceKeyboardInset(s, { type: "didHide" });
      expect(hide.toValue).toBe(0);
      expect(hide.hard).toBe(true);
      s = hide.state;
    }
    expect(s.inset).toBe(0);
    expect(s.viewBottomY).toBe(852);
  });

  it("zero duration falls back to 250", () => {
    const r = reduceKeyboardInset(base, { type: "show", keyboardTopY: 600, duration: 0 });
    expect(r.duration).toBe(250);
  });

  it("initialKeyboardInsetState starts at zero", () => {
    const s = initialKeyboardInsetState();
    expect(s.inset).toBe(0);
    expect(s.viewBottomY).toBe(0);
  });
});
