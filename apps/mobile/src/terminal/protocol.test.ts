import { describe, expect, it } from "vitest";

import {
  TERMINAL_REPLAY_CAP_BYTES,
  encodeHostMessage,
  encodeViewMessage,
  isDecimalU64,
  parseHostMessage,
  parseViewMessage,
  type HostMessage,
  type ViewMessage,
} from "./protocol";

describe("host messages", () => {
  it("round-trips terminal size choices and validates mismatch notifications", () => {
    for (const type of ["updateSize", "dismissSize"] as const) {
      expect(parseHostMessage(encodeHostMessage({ type }))).toEqual({ type });
    }
    expect(parseViewMessage(encodeViewMessage({ type: "sizeMismatch", show: true }))).toEqual({ type: "sizeMismatch", show: true });
    expect(parseViewMessage({ type: "sizeMismatch", show: "yes" })).toBeNull();
  });
  it("round-trips init with u64 ids above the float53 range", () => {
    const init: HostMessage = {
      type: "init",
      socketBaseUrl: "wss://pm.example",
      terminalId: "18446744073709551615",
      generation: "9007199254740993",
      ticket: "t-abc",
      replayCapBytes: TERMINAL_REPLAY_CAP_BYTES,
      fontSize: 14,
      theme: { background: "#101014", foreground: "#e2e2e6" },
    };
    const parsed = parseHostMessage(encodeHostMessage(init));
    expect(parsed).toEqual(init);
    expect(parsed && parsed.type === "init" && parsed.terminalId).toBe("18446744073709551615");
  });

  it("trims trailing slashes from the socket base URL", () => {
    const parsed = parseHostMessage(
      encodeHostMessage({
        type: "init",
        socketBaseUrl: "ws://10.0.0.5:8080/",
        terminalId: "7",
        generation: "0",
        replayCapBytes: 1024,
        fontSize: 12,
      }),
    );
    expect(parsed && parsed.type === "init" && parsed.socketBaseUrl).toBe("ws://10.0.0.5:8080");
  });

  it("round-trips write, setVisible, pan, touch, refit, and shutdown", () => {
    const messages: HostMessage[] = [
      { type: "write", dataBase64: "Gw==" },
      { type: "setVisible", visible: false },
      { type: "pan", phase: "start", x: 50, y: 120.5, timeMs: 1234.5 },
      { type: "pan", phase: "move", x: 50, y: 180, timeMs: 1301 },
      { type: "pan", phase: "end", x: 50, y: 180, timeMs: 1340 },
      { type: "pan", phase: "cancel", x: 50, y: 180, timeMs: 1350 },
      { type: "touch", phase: "start", x: 12.5, y: 40, timeMs: 100, touchCount: 1 },
      { type: "touch", phase: "move", x: 60, y: 41, timeMs: 130, touchCount: 1 },
      { type: "touch", phase: "end", x: 60, y: 41, timeMs: 160, touchCount: 1 },
      { type: "touch", phase: "cancel", x: 60, y: 41, timeMs: 170, touchCount: 2 },
      { type: "refit" },
      { type: "requestDiag" },
      { type: "scrollToBottom" },
      { type: "getSelection" },
      { type: "cancelTouch" },
      { type: "finishSelectionTouch" },
      { type: "shutdown" },
    ];
    for (const msg of messages) {
      expect(parseHostMessage(encodeHostMessage(msg))).toEqual(msg);
    }
  });

  it("rejects malformed input", () => {
    expect(parseHostMessage(undefined)).toBeNull();
    expect(parseHostMessage("not json")).toBeNull();
    expect(parseHostMessage(JSON.stringify(["init"]))).toBeNull();
    expect(parseHostMessage(JSON.stringify({ type: "unknown" }))).toBeNull();
    expect(parseHostMessage(JSON.stringify({ type: "write", dataBase64: 5 }))).toBeNull();
    const base = {
      type: "init",
      socketBaseUrl: "wss://pm.example",
      terminalId: "7",
      generation: "1",
      replayCapBytes: 1024,
      fontSize: 14,
    };
    expect(parseHostMessage(JSON.stringify({ ...base, socketBaseUrl: "https://pm.example" }))).toBeNull();
    expect(parseHostMessage(JSON.stringify({ ...base, terminalId: 7 }))).toBeNull();
    expect(parseHostMessage(JSON.stringify({ ...base, generation: "-1" }))).toBeNull();
    expect(parseHostMessage(JSON.stringify({ ...base, replayCapBytes: 0 }))).toBeNull();
    expect(parseHostMessage(JSON.stringify({ ...base, replayCapBytes: 10.5 }))).toBeNull();
    expect(parseHostMessage(JSON.stringify({ ...base, fontSize: "14" }))).toBeNull();
    const pan = { type: "pan", phase: "move", x: 1, y: 2, timeMs: 3 };
    expect(parseHostMessage(JSON.stringify({ ...pan, phase: "hover" }))).toBeNull();
    expect(parseHostMessage(JSON.stringify({ ...pan, x: "1" }))).toBeNull();
    expect(parseHostMessage(JSON.stringify({ ...pan, y: Number.NaN }))).toBeNull();
    expect(parseHostMessage(JSON.stringify({ ...pan, timeMs: undefined }))).toBeNull();
    const touch = { type: "touch", phase: "move", x: 1, y: 2, timeMs: 3, touchCount: 1 };
    expect(parseHostMessage(JSON.stringify({ ...touch, phase: "hover" }))).toBeNull();
    expect(parseHostMessage(JSON.stringify({ ...touch, touchCount: 0 }))).toBeNull();
    expect(parseHostMessage(JSON.stringify({ ...touch, touchCount: 1.5 }))).toBeNull();
    expect(parseHostMessage(JSON.stringify({ ...touch, x: Number.NaN }))).toBeNull();
  });
});

describe("view messages", () => {
  it("round-trips every message shape", () => {
    const messages: ViewMessage[] = [
      { type: "ready" },
      { type: "status", phase: "reconnecting", detail: "terminal connection failed", canRetry: true },
      { type: "status", phase: "online" },
      { type: "replay", bytes: 4096, durationMs: 120, snapshot: true, capped: false },
      {
        type: "metrics",
        outputBytes: 65536,
        firstFrameMs: 42,
        lastEchoMs: 18,
        renderer: "webgl",
        inputBytesSent: 512,
        inputBytesDropped: 6,
        bufferType: "alternate",
        mouseTracking: "any",
      },
      { type: "metrics", outputBytes: 0, firstFrameMs: null, lastEchoMs: null, renderer: "dom" },
      { type: "selection", active: true },
      { type: "selection", active: true, bracketedPasteMode: true },
      { type: "selection", active: false, bracketedPasteMode: false },
      { type: "selectionText", text: "hello world" },
      { type: "title", title: "vim" },
      { type: "link", url: "https://example.com" },
      { type: "error", message: "socket construction failed" },
      { type: "following", following: true },
      { type: "following", following: false },
      {
        type: "keyboardDiag",
        layoutHeightPx: 852,
        visualHeightPx: 500,
        containerHeightPx: 500,
        cols: 80,
        rows: 24,
        terminalPixelHeight: 480,
      },
    ];
    for (const msg of messages) {
      expect(parseViewMessage(encodeViewMessage(msg))).toEqual(msg);
    }
  });

  it("rejects malformed input", () => {
    expect(parseViewMessage("{}")).toBeNull();
    expect(parseViewMessage(JSON.stringify({ type: "status", phase: "warp" }))).toBeNull();
    expect(parseViewMessage(JSON.stringify({ type: "replay", bytes: "4096" }))).toBeNull();
    expect(parseViewMessage(JSON.stringify({ type: "metrics", outputBytes: 1, firstFrameMs: "x", lastEchoMs: null, renderer: "dom" }))).toBeNull();
    expect(parseViewMessage(JSON.stringify({ type: "metrics", outputBytes: 1, firstFrameMs: null, lastEchoMs: null, renderer: "dom", bufferType: "primary" }))).toBeNull();
    expect(parseViewMessage(JSON.stringify({ type: "metrics", outputBytes: 1, firstFrameMs: null, lastEchoMs: null, renderer: "dom", mouseTracking: "all" }))).toBeNull();
    expect(parseViewMessage(JSON.stringify({ type: "selection", active: "yes" }))).toBeNull();
    expect(parseViewMessage(JSON.stringify({ type: "following", following: "yes" }))).toBeNull();
    expect(parseViewMessage(JSON.stringify({ type: "following" }))).toBeNull();
  });
});

describe("isDecimalU64", () => {
  it("accepts canonical decimals within u64", () => {
    expect(isDecimalU64("0")).toBe(true);
    expect(isDecimalU64("18446744073709551615")).toBe(true);
  });

  it("rejects padding, signs, overflow, and non-strings", () => {
    expect(isDecimalU64("01")).toBe(false);
    expect(isDecimalU64("+1")).toBe(false);
    expect(isDecimalU64("18446744073709551616")).toBe(false);
    expect(isDecimalU64(7)).toBe(false);
  });
});
