import { describe, expect, it } from "vitest";

import {
  followingBottom,
  linesFromBottom,
  terminalHeightPx,
  viewportForLinesFromBottom,
  viewportRestoreDelta,
} from "./keyboardViewport";

describe("terminalHeightPx", () => {
  it("uses the layout viewport when no visual viewport is available", () => {
    expect(terminalHeightPx({ layoutHeightPx: 812, visualHeightPx: null })).toBe(812);
  });

  it("shrinks to the visual viewport when the keyboard overlays the page", () => {
    expect(terminalHeightPx({ layoutHeightPx: 812, visualHeightPx: 476 })).toBe(476);
  });

  it("restores the full height when the keyboard hides", () => {
    expect(terminalHeightPx({ layoutHeightPx: 812, visualHeightPx: 812 })).toBe(812);
  });

  it("never exceeds the layout viewport", () => {
    expect(terminalHeightPx({ layoutHeightPx: 400, visualHeightPx: 900 })).toBe(400);
  });

  it("floors fractional viewport heights", () => {
    expect(terminalHeightPx({ layoutHeightPx: 812, visualHeightPx: 476.7 })).toBe(476);
  });

  it("ignores degenerate visual viewport readings", () => {
    expect(terminalHeightPx({ layoutHeightPx: 812, visualHeightPx: 0 })).toBe(812);
    expect(terminalHeightPx({ layoutHeightPx: 812, visualHeightPx: Number.NaN })).toBe(812);
    expect(terminalHeightPx({ layoutHeightPx: -5, visualHeightPx: null })).toBe(0);
  });
});

describe("followingBottom", () => {
  it("is true at the bottom of the scrollback", () => {
    expect(followingBottom(120, 120)).toBe(true);
  });

  it("is false while scrolled up into the scrollback", () => {
    expect(followingBottom(80, 120)).toBe(false);
  });

  it("is true on the alternate screen, which has no scrollback", () => {
    expect(followingBottom(0, 0)).toBe(true);
  });
});

describe("viewportRestoreDelta", () => {
  it("returns 0 when xterm preserved the viewport position", () => {
    // Saved at line 50, xterm stayed at 50, baseY still allows it.
    expect(viewportRestoreDelta(50, 50, 120)).toBe(0);
  });

  it("corrects when xterm reset the viewport to the top", () => {
    // Saved at line 50, xterm reset to 0 after refit.
    expect(viewportRestoreDelta(50, 0, 120)).toBe(50);
  });

  it("clamps to the new baseY when scrollback shrunk", () => {
    // Saved at line 100 but the refit increased rows so baseY dropped to 80.
    expect(viewportRestoreDelta(100, 0, 80)).toBe(80);
  });

  it("returns 0 when saved position equals current after clamp", () => {
    expect(viewportRestoreDelta(80, 80, 80)).toBe(0);
  });

  it("handles the alternate screen where baseY is 0", () => {
    // On the alt screen both saved and current are 0 — no correction.
    expect(viewportRestoreDelta(0, 0, 0)).toBe(0);
  });
});

describe("replay viewport bookmarks", () => {
  it("keeps a following reader at the rebuilt bottom", () => {
    expect(linesFromBottom(120, 120)).toBe(0);
    expect(viewportForLinesFromBottom(900, 0)).toBe(900);
  });

  it("keeps a scrolled reader the same distance from the bottom", () => {
    const bookmark = linesFromBottom(80, 120);
    expect(bookmark).toBe(40);
    expect(viewportForLinesFromBottom(900, bookmark)).toBe(860);
  });

  it("clamps a bookmark when rebuilt scrollback is shorter", () => {
    expect(viewportForLinesFromBottom(20, 40)).toBe(0);
  });
});
