import { describe, expect, it } from "vitest";
import { decodeOsc52 } from "./osc52";

function b64(s: string): string {
  const bytes = new TextEncoder().encode(s);
  let binary = "";
  for (const byte of bytes) binary += String.fromCharCode(byte);
  return btoa(binary);
}

describe("decodeOsc52", () => {
  it("decodes a base64 write payload to text", () => {
    expect(decodeOsc52(`c;${b64("hello world")}`)).toBe("hello world");
  });

  it("handles multi-byte UTF-8", () => {
    expect(decodeOsc52(`c;${b64("café — 日本")}`)).toBe("café — 日本");
  });

  it("ignores read requests so the clipboard cannot be exfiltrated", () => {
    expect(decodeOsc52("c;?")).toBeNull();
  });

  it("returns null for malformed or empty sequences", () => {
    expect(decodeOsc52("no-separator")).toBeNull();
    expect(decodeOsc52("c;")).toBeNull();
    expect(decodeOsc52("c;!!!not base64!!!")).toBeNull();
  });
});
