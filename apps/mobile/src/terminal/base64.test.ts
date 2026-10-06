import { describe, expect, it } from "vitest";

import { base64ToBytes, bytesToBase64 } from "./base64";

describe("base64", () => {
  it("round-trips binary data at every padding length", () => {
    for (const length of [0, 1, 2, 3, 4, 5, 255]) {
      const bytes = new Uint8Array(length).map((_v, i) => (i * 37 + length) & 0xff);
      const encoded = bytesToBase64(bytes);
      expect(encoded).toBe(Buffer.from(bytes).toString("base64"));
      expect(Array.from(base64ToBytes(encoded) ?? [])).toEqual(Array.from(bytes));
    }
  });

  it("encodes control bytes used by the accessory row", () => {
    expect(bytesToBase64(new Uint8Array([0x1b]))).toBe("Gw==");
    expect(bytesToBase64(new Uint8Array([0x03]))).toBe("Aw==");
  });

  it("rejects invalid input", () => {
    expect(base64ToBytes("a")).toBeNull();
    expect(base64ToBytes("ab!d")).toBeNull();
    expect(base64ToBytes("=abc")).toBeNull();
  });
});
