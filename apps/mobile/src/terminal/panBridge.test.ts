import { describe, expect, it } from "vitest";

import { TerminalPanBridge, type PanSample } from "./panBridge";
import type { HostMessage } from "./protocol";

function sample(y: number, timeMs: number, x = 50, touchCount = 1): PanSample {
  return { x, y, timeMs, touchCount };
}

function bridge() {
  const sent: HostMessage[] = [];
  return { bridge: new TerminalPanBridge((msg) => sent.push(msg)), sent };
}

// Drives the responder callbacks the way React Native does: should-claim on
// every unclaimed move, then grant and move once claimed.
function drag(b: TerminalPanBridge, samples: PanSample[]): boolean {
  b.touchStart(samples[0]);
  let claimed = false;
  for (const s of samples.slice(1)) {
    if (!claimed && b.shouldClaim(s)) {
      claimed = true;
      b.grant(s);
    } else if (claimed) {
      b.move(s);
    }
  }
  return claimed;
}

describe("TerminalPanBridge", () => {
  it("claims a vertical drag and forwards it from its origin", () => {
    const { bridge: b, sent } = bridge();
    const claimed = drag(b, [sample(100, 0), sample(120, 30), sample(150, 60)]);
    expect(claimed).toBe(true);
    expect(sent).toEqual([
      { type: "pan", phase: "start", x: 50, y: 100, timeMs: 0 },
      { type: "pan", phase: "move", x: 50, y: 120, timeMs: 30 },
      { type: "pan", phase: "move", x: 50, y: 150, timeMs: 60 },
    ]);
  });

  it("ends the forwarded pan on release", () => {
    const { bridge: b, sent } = bridge();
    drag(b, [sample(100, 0), sample(140, 30)]);
    b.release(sample(140, 80));
    expect(sent[sent.length - 1]).toEqual({ type: "pan", phase: "end", x: 50, y: 140, timeMs: 80 });
  });

  it("does not claim movement below the drag slop", () => {
    const { bridge: b, sent } = bridge();
    b.touchStart(sample(100, 0));
    expect(b.shouldClaim(sample(105, 30))).toBe(false);
    b.release(sample(105, 60));
    expect(sent).toEqual([]);
  });

  it("claims horizontal drags as scrolls and forwards them from the origin", () => {
    const { bridge: b, sent } = bridge();
    const claimed = drag(b, [sample(100, 0), sample(105, 30, 90), sample(110, 60, 130)]);
    expect(claimed).toBe(true);
    expect(sent).toEqual([
      { type: "pan", phase: "start", x: 50, y: 100, timeMs: 0 },
      { type: "pan", phase: "move", x: 90, y: 105, timeMs: 30 },
      { type: "pan", phase: "move", x: 130, y: 110, timeMs: 60 },
    ]);
  });

  it("leaves long-press drags to WebView selection", () => {
    const { bridge: b, sent } = bridge();
    b.touchStart(sample(100, 0));
    expect(b.shouldClaim(sample(150, 600))).toBe(false);
    expect(b.shouldClaim(sample(200, 650))).toBe(false);
    expect(sent).toEqual([]);
  });

  it("never claims a gesture that starts with multiple touches", () => {
    const { bridge: b, sent } = bridge();
    b.touchStart(sample(100, 0, 50, 2));
    expect(b.shouldClaim(sample(150, 50, 50, 2))).toBe(false);
    expect(b.shouldClaim(sample(200, 100))).toBe(false);
    expect(sent).toEqual([]);
  });

  it("cancels the forwarded pan when a second finger joins a claimed drag", () => {
    const { bridge: b, sent } = bridge();
    drag(b, [sample(100, 0), sample(140, 30)]);
    b.move(sample(150, 60, 50, 2));
    expect(sent[sent.length - 1]).toEqual({ type: "pan", phase: "cancel", x: 50, y: 150, timeMs: 60 });
    const afterCancel = sent.length;
    b.move(sample(200, 90));
    b.release(sample(200, 120));
    expect(sent.length).toBe(afterCancel);
  });

  it("cancels the forwarded pan when a second finger lands as a new responder start", () => {
    const { bridge: b, sent } = bridge();
    drag(b, [sample(100, 0), sample(140, 30)]);
    b.touchStart(sample(150, 60, 80, 2));
    expect(sent[sent.length - 1]).toEqual({ type: "pan", phase: "cancel", x: 80, y: 150, timeMs: 60 });
  });

  it("cancels the forwarded pan when the responder is terminated", () => {
    const { bridge: b, sent } = bridge();
    drag(b, [sample(100, 0), sample(140, 30)]);
    b.terminate(sample(140, 60));
    expect(sent[sent.length - 1]).toEqual({ type: "pan", phase: "cancel", x: 50, y: 140, timeMs: 60 });
  });

  it("sends nothing on an unclaimed release or termination", () => {
    const { bridge: b, sent } = bridge();
    b.touchStart(sample(100, 0));
    b.release(sample(100, 40));
    b.touchStart(sample(100, 50));
    b.terminate(sample(100, 90));
    expect(sent).toEqual([]);
  });

  it("claims a fresh drag after an aborted one", () => {
    const { bridge: b, sent } = bridge();
    drag(b, [sample(100, 0), sample(140, 30)]);
    b.terminate(sample(140, 60));
    sent.length = 0;
    const claimed = drag(b, [sample(200, 100), sample(240, 130)]);
    expect(claimed).toBe(true);
    expect(sent[0]).toEqual({ type: "pan", phase: "start", x: 50, y: 200, timeMs: 100 });
  });
});
