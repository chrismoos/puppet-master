import { describe, expect, it, vi } from "vitest";
import { SESSION_DROP_FILE_MAX } from "../api/drops";
import { guardStrayFileDrops } from "../fileDropGuard";
import { dropSelection, dropStatusText, isFileDrag, skippedReason } from "./sessionDrop";

function file(name: string, size: number): File {
  const blob = new Blob([new Uint8Array(size)]);
  return new File([blob], name);
}

describe("isFileDrag", () => {
  it("claims a file drag and leaves the app's own drags alone", () => {
    expect(isFileDrag(["Files"])).toBe(true);
    expect(isFileDrag(["text/plain", "Files"])).toBe(true);
    expect(isFileDrag(["application/x-pm-session"])).toBe(false);
    expect(isFileDrag([])).toBe(false);
  });
});

describe("dropSelection", () => {
  it("takes files and sets aside folders and oversized ones", () => {
    const selection = dropSelection(
      [file("shot.png", 10), file("folder", 0), file("huge.bin", SESSION_DROP_FILE_MAX + 1)],
      [
        { kind: "file" },
        { kind: "file", webkitGetAsEntry: () => ({ isDirectory: true }) },
        { kind: "file" },
      ],
    );
    expect(selection.files.map((f) => f.name)).toEqual(["shot.png"]);
    expect(selection.directories).toBe(1);
    expect(selection.oversized).toEqual(["huge.bin"]);
  });

  it("treats a folder as one even where the entry API is missing", () => {
    const selection = dropSelection([file("folder", 0)]);
    expect(selection.files).toEqual([]);
    expect(selection.directories).toBe(1);
  });

  it("says what it skipped", () => {
    const selection = dropSelection([file("folder", 0), file("huge.bin", SESSION_DROP_FILE_MAX + 1)]);
    expect(skippedReason(selection)).toBe("a folder was skipped, huge.bin is over the 10 MB limit");
    expect(skippedReason(dropSelection([file("shot.png", 1)]))).toBe("");
  });
});

describe("dropStatusText", () => {
  it("counts a multi-file drop and names the file in flight", () => {
    expect(dropStatusText({ kind: "idle" })).toBe("");
    expect(dropStatusText({ kind: "over" })).toBe("Drop here to insert file path for agent");
    expect(dropStatusText({ kind: "uploading", name: "a.png", done: 0, total: 1 })).toBe(
      "Storing a.png…",
    );
    expect(dropStatusText({ kind: "uploading", name: "b.png", done: 1, total: 3 })).toBe(
      "Storing b.png (2 of 3)…",
    );
    expect(dropStatusText({ kind: "failed", reason: "too big" })).toBe("too big");
  });
});

describe("guardStrayFileDrops", () => {
  function listeners() {
    const handlers = new Map<string, (event: unknown) => void>();
    return {
      doc: {
        addEventListener: (type: string, listener: (event: never) => void) =>
          handlers.set(type, listener as (event: unknown) => void),
        removeEventListener: (type: string) => handlers.delete(type),
      },
      handlers,
    };
  }

  it("stops a stray file drop from navigating the tab away", () => {
    const { doc, handlers } = listeners();
    const dispose = guardStrayFileDrops(doc as never);
    const preventDefault = vi.fn();
    handlers.get("drop")?.({
      dataTransfer: { types: ["Files"] },
      defaultPrevented: false,
      preventDefault,
    });
    expect(preventDefault).toHaveBeenCalled();
    dispose();
    expect(handlers.size).toBe(0);
  });

  it("leaves a drop a terminal already took, and a drag of the app's own", () => {
    const { doc, handlers } = listeners();
    guardStrayFileDrops(doc as never);
    const taken = vi.fn();
    handlers.get("drop")?.({
      dataTransfer: { types: ["Files"] },
      defaultPrevented: true,
      preventDefault: taken,
    });
    expect(taken).not.toHaveBeenCalled();

    const ownDrag = vi.fn();
    handlers.get("dragover")?.({
      dataTransfer: { types: ["application/x-pm-session"] },
      defaultPrevented: false,
      preventDefault: ownDrag,
    });
    expect(ownDrag).not.toHaveBeenCalled();
  });
});
