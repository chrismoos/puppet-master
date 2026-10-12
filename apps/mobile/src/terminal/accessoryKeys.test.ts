import { describe, expect, it } from "vitest";
import { accessoryKeyBytes, afterKeyPress, ctrlLetterBytes, cycleModifier, type Modifiers } from "./accessoryKeys";

const none: Modifiers = { shift: false, alt: false, ctrl: false };
const text = (bytes: number[]) => String.fromCharCode(...bytes);

describe("accessoryKeyBytes", () => {
  it("sends plain sequences without modifiers", () => {
    expect(text(accessoryKeyBytes("up", none))).toBe("\x1b[A");
    expect(text(accessoryKeyBytes("left", none))).toBe("\x1b[D");
    expect(text(accessoryKeyBytes("tab", none))).toBe("\t");
    expect(text(accessoryKeyBytes("enter", none))).toBe("\r");
    expect(text(accessoryKeyBytes("esc", none))).toBe("\x1b");
  });

  it("encodes arrow modifiers as xterm CSI 1;m", () => {
    expect(text(accessoryKeyBytes("up", { ...none, shift: true }))).toBe("\x1b[1;2A");
    expect(text(accessoryKeyBytes("left", { ...none, alt: true }))).toBe("\x1b[1;3D");
    expect(text(accessoryKeyBytes("right", { ...none, alt: true, shift: true }))).toBe("\x1b[1;4C");
    expect(text(accessoryKeyBytes("down", { ...none, ctrl: true }))).toBe("\x1b[1;5B");
    expect(text(accessoryKeyBytes("up", { shift: true, alt: true, ctrl: true }))).toBe("\x1b[1;8A");
  });

  it("sends backtab for Shift+Tab and prefixes Alt with ESC", () => {
    expect(text(accessoryKeyBytes("tab", { ...none, shift: true }))).toBe("\x1b[Z");
    expect(text(accessoryKeyBytes("tab", { ...none, alt: true }))).toBe("\x1b\t");
    expect(text(accessoryKeyBytes("tab", { ...none, alt: true, shift: true }))).toBe("\x1b\x1b[Z");
    expect(text(accessoryKeyBytes("enter", { ...none, alt: true }))).toBe("\x1b\r");
  });
});

describe("ctrlLetterBytes", () => {
  it("maps letters to control bytes, with Alt as an ESC prefix", () => {
    expect(ctrlLetterBytes("c")).toEqual([0x03]);
    expect(ctrlLetterBytes("W", { alt: true })).toEqual([0x1b, 0x17]);
  });
});

describe("modifier state", () => {
  it("cycles off → armed → locked → off and spends only armed modifiers", () => {
    expect(cycleModifier("off")).toBe("armed");
    expect(cycleModifier("armed")).toBe("locked");
    expect(cycleModifier("locked")).toBe("off");
    expect(afterKeyPress("armed")).toBe("off");
    expect(afterKeyPress("locked")).toBe("locked");
  });
});
