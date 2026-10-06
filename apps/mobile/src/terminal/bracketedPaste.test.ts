import { describe, expect, it } from "vitest";

import { bytesToBase64, base64ToBytes } from "./base64";

const BRACKETED_PASTE_START = "\x1b[200~";
const BRACKETED_PASTE_END = "\x1b[201~";

function encodePaste(text: string, bracketedPasteMode: boolean): string {
  const encoder = new TextEncoder();
  const payload = bracketedPasteMode
    ? BRACKETED_PASTE_START + text + BRACKETED_PASTE_END
    : text;
  return bytesToBase64(encoder.encode(payload));
}

describe("bracketed paste wrapping", () => {
  it("wraps text in bracket sequences when mode is active", () => {
    const b64 = encodePaste("echo hello", true);
    const bytes = base64ToBytes(b64);
    expect(bytes).not.toBeNull();
    const decoded = new TextDecoder().decode(bytes!);
    expect(decoded).toBe("\x1b[200~echo hello\x1b[201~");
  });

  it("sends text as-is when mode is inactive", () => {
    const b64 = encodePaste("echo hello", false);
    const bytes = base64ToBytes(b64);
    expect(bytes).not.toBeNull();
    const decoded = new TextDecoder().decode(bytes!);
    expect(decoded).toBe("echo hello");
  });

  it("handles multi-line paste correctly", () => {
    const text = "line1\nline2\nline3";
    const b64 = encodePaste(text, true);
    const bytes = base64ToBytes(b64);
    expect(bytes).not.toBeNull();
    const decoded = new TextDecoder().decode(bytes!);
    expect(decoded).toBe("\x1b[200~line1\nline2\nline3\x1b[201~");
    expect(decoded).toContain("\x1b[200~");
    expect(decoded).toContain("\x1b[201~");
  });

  it("handles empty paste", () => {
    const b64 = encodePaste("", true);
    const bytes = base64ToBytes(b64);
    expect(bytes).not.toBeNull();
    const decoded = new TextDecoder().decode(bytes!);
    expect(decoded).toBe("\x1b[200~\x1b[201~");
  });

  it("handles unicode text", () => {
    const b64 = encodePaste("echo \u2603", false);
    const bytes = base64ToBytes(b64);
    expect(bytes).not.toBeNull();
    const decoded = new TextDecoder().decode(bytes!);
    expect(decoded).toBe("echo \u2603");
  });
});
