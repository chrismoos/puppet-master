import { describe, expect, it } from "vitest";
import { OscObserver } from "./oscObserver";

describe("OSC observer", () => {
  it("joins UTF-8 payload bytes split across BEL-terminated writes", () => {
    const seen: string[] = [];
    const observer = new OscObserver((payload) => seen.push(payload));
    const bytes = new TextEncoder().encode("\u001b]777;pm-command;編集");
    const split = bytes.indexOf(0xe7) + 1;
    observer.write(bytes.subarray(0, split));
    observer.write(bytes.subarray(split));
    observer.write(new Uint8Array([0x07]));
    expect(seen).toEqual(["777;pm-command;編集"]);
  });

  it("accepts split string terminators and ignores OSC-like bytes in DCS", () => {
    const seen: string[] = [];
    const observer = new OscObserver((payload) => seen.push(payload));
    observer.write("\u001bPignored \u001b]777;hidden");
    observer.write("\u001b\\\u001b]52;c;clip\u001b");
    observer.write("\\");
    expect(seen).toEqual(["52;c;clip"]);
  });

  it("bounds payload memory and ignores C1 bytes in UTF-8 text", () => {
    const seen: string[] = [];
    const observer = new OscObserver((payload) => seen.push(payload));
    observer.write(new Uint8Array([0x9d, 0x37, 0x37, 0x37, 0x3b, 0x07]));
    observer.write("\u001b]777;" + "x".repeat(65_537) + "\u0007");
    expect(seen).toEqual([]);
  });

  it("recovers after canceled control strings", () => {
    const seen: string[] = [];
    const observer = new OscObserver((payload) => seen.push(payload));
    observer.write(new Uint8Array([0x1b, 0x5d, 0x37, 0x37, 0x37, 0x18]));
    observer.write("\u001b]777;ok\u001b\\");
    expect(seen).toEqual(["777;ok"]);
  });
});
