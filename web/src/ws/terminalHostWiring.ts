import type { IDisposable } from "@xterm/xterm";
import { decodeOsc52 } from "@puppet-master/client-core/ws/osc52";
import type { TerminalClipboard } from "./terminalClipboard";

const FOREGROUND_COMMAND_OSC = 777;
const FOREGROUND_COMMAND_OSC_PREFIX = "pm-command;";
const CLIPBOARD_OSC = 52;

export interface OscTerminal {
  parser: {
    registerOscHandler(id: number, callback: (data: string) => boolean): IDisposable;
  };
}

export interface CopyTerminal extends OscTerminal {
  getSelection(): string;
  hasSelection(): boolean;
  attachCustomKeyEventHandler(handler: (event: KeyboardEvent) => boolean): void;
}

export interface CopyHost {
  addEventListener(type: "mouseup", listener: () => void): void;
  removeEventListener(type: "mouseup", listener: () => void): void;
}

export interface CopyShortcutEvent {
  type: string;
  metaKey: boolean;
  ctrlKey: boolean;
  shiftKey: boolean;
  key: string;
}

/** The daemon's foreground-command report for a terminal. */
export function registerForegroundCommandHandler(
  term: OscTerminal,
  onCommand: (command: string) => void,
): IDisposable {
  return term.parser.registerOscHandler(FOREGROUND_COMMAND_OSC, (data) => {
    if (!data.startsWith(FOREGROUND_COMMAND_OSC_PREFIX)) return false;
    onCommand(data.slice(FOREGROUND_COMMAND_OSC_PREFIX.length));
    return true;
  });
}

export function isCopyShortcut(event: CopyShortcutEvent): boolean {
  if (event.type !== "keydown") return false;
  return (event.metaKey && event.key === "c")
    || (event.ctrlKey && event.shiftKey && event.key.toLowerCase() === "c");
}

/**
 * Every xterm host gets the same three copy paths from here: select-to-copy
 * (xterm keeps its selection outside the DOM, so a native browser copy never
 * sees it), the explicit copy shortcut, and the application's own OSC 52
 * copies, which is how Claude Code and Codex copy over a mouse-tracking TUI.
 * OSC 52 read requests are refused by decodeOsc52.
 */
export function wireTerminalClipboard(term: CopyTerminal, host: CopyHost, clipboard: TerminalClipboard): () => void {
  const copySelection = () => {
    const selection = term.getSelection();
    if (selection) void clipboard.copySelection(selection);
  };
  host.addEventListener("mouseup", copySelection);
  const osc = term.parser.registerOscHandler(CLIPBOARD_OSC, (data) => {
    const text = decodeOsc52(data);
    if (text === null) return false;
    void clipboard.copyFromApplication(text);
    return true;
  });
  term.attachCustomKeyEventHandler((event) => {
    if (!isCopyShortcut(event) || !term.hasSelection()) return true;
    copySelection();
    return false;
  });
  return () => {
    host.removeEventListener("mouseup", copySelection);
    osc.dispose();
  };
}
