export type ModifierState = "off" | "armed" | "locked";

export function cycleModifier(state: ModifierState): ModifierState {
  switch (state) {
    case "off": return "armed";
    case "armed": return "locked";
    case "locked": return "off";
  }
}

/** A one-shot modifier is spent by the key it modified, a locked one stays. */
export function afterKeyPress(state: ModifierState): ModifierState {
  return state === "armed" ? "off" : state;
}

export interface Modifiers {
  shift: boolean;
  alt: boolean;
  ctrl: boolean;
}

// A modifier is not a bit on the byte: Shift+Tab is CSI Z rather than 0x09
// with a flag. Arrows carry modifiers as CSI 1;m where m = 1 + shift +
// 2·alt + 4·ctrl, and a key with no parameter slot takes Alt as an ESC
// prefix, which is what readline and most TUIs expect.

const ESC = 0x1b;
const CSI = [ESC, 0x5b];

export type AccessoryKeyId = "esc" | "tab" | "left" | "down" | "up" | "right" | "enter";

export interface AccessoryKey {
  id: AccessoryKeyId;
  label: string;
}

export const ACCESSORY_KEYS: AccessoryKey[] = [
  { id: "esc", label: "Esc" },
  { id: "tab", label: "Tab" },
  { id: "left", label: "◀" },
  { id: "down", label: "▼" },
  { id: "up", label: "▲" },
  { id: "right", label: "▶" },
  { id: "enter", label: "Enter" },
];

const ARROW_FINAL: Partial<Record<AccessoryKeyId, number>> = {
  up: 0x41,    // A
  down: 0x42,  // B
  right: 0x43, // C
  left: 0x44,  // D
};

function modifierParam(mods: Modifiers): number {
  return 1 + (mods.shift ? 1 : 0) + (mods.alt ? 2 : 0) + (mods.ctrl ? 4 : 0);
}

function withAlt(mods: Modifiers, bytes: number[]): number[] {
  return mods.alt ? [ESC, ...bytes] : bytes;
}

/** Resolves the byte sequence a key sends under the active modifiers. */
export function accessoryKeyBytes(id: AccessoryKeyId, mods: Modifiers): number[] {
  const arrow = ARROW_FINAL[id];
  if (arrow !== undefined) {
    const param = modifierParam(mods);
    if (param === 1) return [...CSI, arrow];
    return [...CSI, 0x31, 0x3b, ...String(param).split("").map((c) => c.charCodeAt(0)), arrow];
  }
  switch (id) {
    case "tab":
      // Shift+Tab is backtab (CSI Z). Ctrl has no conventional encoding.
      return withAlt(mods, mods.shift ? [...CSI, 0x5a] : [0x09]);
    case "enter":
      // Alt+Enter (ESC CR) is the newline-without-submit chord agent CLIs use.
      return withAlt(mods, [0x0d]);
    case "esc":
      return withAlt(mods, [ESC]);
    default:
      return [];
  }
}

// Ctrl + letter = byte value minus 0x40 (e.g. C → 0x03).
export const CTRL_LETTER_KEYS = ["C", "D", "Z", "A", "E", "L", "R", "W", "U", "K"] as const;

export function ctrlLetterBytes(letter: string, mods: Pick<Modifiers, "alt"> = { alt: false }): number[] {
  const byte = letter.toUpperCase().charCodeAt(0) - 0x40;
  return mods.alt ? [ESC, byte] : [byte];
}
