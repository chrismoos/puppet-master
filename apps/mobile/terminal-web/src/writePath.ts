// The write path from xterm to the PTY socket. Every byte xterm emits —
// keystrokes, pastes, and the mouse reports it synthesizes from real mouse
// events — must reach the socket unmodified. No layer here may filter or
// rewrite terminal output: an earlier release dropped mouse-report chunks on
// this path and mouse-tracking TUIs silently lost every click.

export interface TerminalOutputSource {
  onData(listener: (data: string) => void): unknown;
  onBinary(listener: (data: string) => void): unknown;
}

export interface TerminalInputSink {
  sendInput(bytes: Uint8Array, submitted?: boolean): void;
}

const CARRIAGE_RETURN = "\r";

export function connectTerminalOutput(term: TerminalOutputSource, sink: TerminalInputSink): void {
  const encoder = new TextEncoder();
  term.onData((text) => sink.sendInput(encoder.encode(text), text === CARRIAGE_RETURN));
  term.onBinary((chunk) => {
    const bytes = new Uint8Array(chunk.length);
    for (let i = 0; i < chunk.length; i += 1) bytes[i] = chunk.charCodeAt(i);
    sink.sendInput(bytes);
  });
}
