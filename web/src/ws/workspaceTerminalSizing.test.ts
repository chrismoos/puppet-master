import { describe, expect, it } from "vitest";
import { holdInput } from "./terminalLayer";
import { workspaceInputDisposition, workspaceInputReady, workspacePtyEchoDisposition, workspaceResizeRequest } from "./workspaceTerminalSizing";

const readyInput = {
  initialSnapshotParsed: true,
  pendingWrites: 0,
  pendingEchoedSize: null,
  requestedSize: { cols: 139, rows: 47 },
  echoedSize: { cols: 139, rows: 47 },
  promptBlocked: false,
  allowMissingEcho: false,
};

describe("workspace terminal input readiness", () => {
  it("waits until the initial snapshot has parsed", () => {
    expect(workspaceInputReady({ ...readyInput, initialSnapshotParsed: false })).toBe(false);
  });

  it("waits for queued terminal writes to finish", () => {
    expect(workspaceInputReady({ ...readyInput, pendingWrites: 1 })).toBe(false);
  });

  it("waits until an echoed resize snapshot has parsed", () => {
    expect(workspaceInputReady({ ...readyInput, pendingEchoedSize: { cols: 120, rows: 40 } })).toBe(false);
  });

  it("waits until the latest requested size is echoed", () => {
    expect(workspaceInputReady({ ...readyInput, echoedSize: { cols: 80, rows: 24 } })).toBe(false);
  });

  it("allows input while a remote owner has blocked local resizing", () => {
    expect(workspaceInputReady({ ...readyInput, echoedSize: { cols: 80, rows: 24 }, promptBlocked: true })).toBe(true);
  });

  it("keeps previously held bytes ahead of new input when remote ownership unblocks the queue", () => {
    const encoder = new TextEncoder();
    const held = holdInput(
      [{ data: encoder.encode("older"), submitted: false }],
      { data: encoder.encode("newer"), submitted: true },
      64 * 1024,
    );
    expect(workspaceInputReady({ ...readyInput, echoedSize: { cols: 80, rows: 24 }, promptBlocked: true })).toBe(true);
    expect(held.map((entry) => new TextDecoder().decode(entry.data))).toEqual(["older", "newer"]);
  });

  it("allows a legacy stream that has no size echo", () => {
    expect(workspaceInputReady({ ...readyInput, echoedSize: null, allowMissingEcho: true })).toBe(true);
  });

  it("rejects a missing size echo when the stream requires one", () => {
    expect(workspaceInputReady({ ...readyInput, echoedSize: null })).toBe(false);
  });

  it("sends a ready oversized paste directly instead of subjecting it to the held-input cap", () => {
    expect(workspaceInputDisposition(true, 0)).toBe("send");
  });

  it("holds new input behind queued bytes even when the terminal is ready", () => {
    expect(workspaceInputDisposition(true, 1)).toBe("hold");
  });

  it("preserves a single atomic paste while input is waiting for terminal readiness", () => {
    const paste = new Uint8Array(64 * 1024 + 1);
    const held = holdInput([], { data: paste, submitted: false }, 64 * 1024);
    expect(workspaceInputDisposition(false, held.length)).toBe("hold");
    expect(held[0].data).toBe(paste);
  });
});

describe("workspace terminal size requests", () => {
  it("requests measured host geometry while the PTY and emulator still match the old size", () => {
    expect(
      workspaceResizeRequest(
        { cols: 139, rows: 47 },
        { cols: 80, rows: 24 },
        { cols: 80, rows: 24 },
      ),
    ).toEqual({ cols: 139, rows: 47 });
  });

  it("does not repeat a request while waiting for the PTY echo", () => {
    expect(
      workspaceResizeRequest(
        { cols: 139, rows: 47 },
        { cols: 139, rows: 47 },
        { cols: 80, rows: 24 },
      ),
    ).toBeNull();
  });

  it("does not request a size the PTY has already echoed", () => {
    expect(
      workspaceResizeRequest(
        { cols: 139, rows: 47 },
        { cols: 80, rows: 24 },
        { cols: 139, rows: 47 },
      ),
    ).toBeNull();
  });
});

describe("workspace PTY size echoes", () => {
  it("does not request a snapshot for an initial echo before the first replay paints", () => {
    expect(workspacePtyEchoDisposition(
      { cols: 80, rows: 24 }, null, { cols: 139, rows: 47 }, true, false,
    )).toBe("apply");
  });

  it("does not request another snapshot for an echo matching the current grid", () => {
    expect(workspacePtyEchoDisposition(
      { cols: 139, rows: 47 }, null, { cols: 139, rows: 47 }, true, true,
    )).toBe("current");
  });

  it("does not duplicate a pending snapshot for the same size echo", () => {
    expect(workspacePtyEchoDisposition(
      { cols: 139, rows: 47 }, { cols: 80, rows: 24 }, { cols: 80, rows: 24 }, true, true,
    )).toBe("pending");
  });

  it("requests a synchronized snapshot before changing an active painted grid", () => {
    expect(workspacePtyEchoDisposition(
      { cols: 139, rows: 47 }, null, { cols: 80, rows: 24 }, true, true,
    )).toBe("snapshot");
  });

  it("applies a changed echo immediately while inactive", () => {
    expect(workspacePtyEchoDisposition(
      { cols: 139, rows: 47 }, null, { cols: 80, rows: 24 }, false, true,
    )).toBe("apply");
  });
});
