import { describe, expect, it } from "vitest";

import { installTextCodecPolyfill, utf8Decode, utf8Encode } from "./textCodec";

describe("utf8 codec", () => {
  it("round-trips ascii, multibyte, and astral text", () => {
    for (const text of ["", "hello", "héllo wörld", "日本語", "emoji 🎛️🧵", "mixed 𝕊 text"]) {
      const encoded = utf8Encode(text);
      expect(Array.from(encoded)).toEqual(Array.from(new TextEncoder().encode(text)));
      expect(utf8Decode(encoded)).toBe(text);
    }
  });

  it("replaces unpaired surrogates on encode", () => {
    const lone = "a\ud800b";
    expect(utf8Decode(utf8Encode(lone))).toBe("a�b");
  });

  it("replaces invalid and truncated sequences on decode", () => {
    expect(utf8Decode(new Uint8Array([0x61, 0xff, 0x62]))).toBe("a�b");
    expect(utf8Decode(new Uint8Array([0xe2, 0x82]))).toBe("�");
    expect(utf8Decode(new Uint8Array([0xc0, 0xaf]))).toBe("�");
  });

  it("installs globals only when missing", () => {
    const scope: Record<string, unknown> = {};
    installTextCodecPolyfill(scope);
    const encoder = new (scope["TextEncoder"] as new () => { encode(text: string): Uint8Array })();
    expect(Array.from(encoder.encode("hé"))).toEqual([0x68, 0xc3, 0xa9]);
    const existing = function Existing(): void {};
    const scope2: Record<string, unknown> = { TextEncoder: existing, TextDecoder: existing };
    installTextCodecPolyfill(scope2);
    expect(scope2["TextEncoder"]).toBe(existing);
    expect(scope2["TextDecoder"]).toBe(existing);
  });
});
