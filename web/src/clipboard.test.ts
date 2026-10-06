import { describe, expect, it, vi } from "vitest";
import { ClipboardUnavailableError } from "@puppet-master/client-core/ws/clipboard";
import { copyTextToClipboard, execCommandCopy, type CopyCommandDocument, type CopyCommandTextArea } from "./clipboard";

function fakeDocument(result: boolean | "throw") {
  const areas: Array<CopyCommandTextArea & { removed: boolean; selected: boolean }> = [];
  const appended: unknown[] = [];
  const doc: CopyCommandDocument = {
    body: { appendChild: (node: unknown) => appended.push(node) },
    createElement: () => {
      const area = {
        value: "",
        attributes: {} as Record<string, string>,
        setAttribute(name: string, value: string) { this.attributes[name] = value; },
        style: { position: "", left: "", top: "", opacity: "" },
        selected: false,
        removed: false,
        select() { this.selected = true; },
        remove() { this.removed = true; },
      };
      areas.push(area);
      return area;
    },
    execCommand: vi.fn(() => {
      if (result === "throw") throw new Error("command refused");
      return result;
    }),
  };
  return { doc, areas, appended };
}

describe("execCommandCopy", () => {
  it("selects the text in a temporary textarea and reports the command's result", () => {
    const { doc, areas, appended } = fakeDocument(true);
    expect(execCommandCopy(doc, "legacy")).toBe(true);
    expect(areas).toHaveLength(1);
    expect(areas[0].value).toBe("legacy");
    expect(areas[0].selected).toBe(true);
    expect(areas[0].removed).toBe(true);
    expect(appended).toEqual([areas[0]]);
    expect(doc.execCommand).toHaveBeenCalledWith("copy");
  });

  it("returns false when the command is refused, throws, or does not exist", () => {
    expect(execCommandCopy(fakeDocument(false).doc, "x")).toBe(false);
    const thrower = fakeDocument("throw");
    expect(execCommandCopy(thrower.doc, "x")).toBe(false);
    expect(thrower.areas[0].removed).toBe(true);
    expect(execCommandCopy({ ...fakeDocument(true).doc, execCommand: undefined }, "x")).toBe(false);
    expect(execCommandCopy(undefined, "x")).toBe(false);
  });
});

describe("copyTextToClipboard", () => {
  it("uses the async clipboard API when the origin has one", async () => {
    const writeText = vi.fn(() => Promise.resolve());
    const { doc } = fakeDocument(true);
    await copyTextToClipboard("secure", { clipboard: { writeText } }, doc);
    expect(writeText).toHaveBeenCalledWith("secure");
    expect(doc.execCommand).not.toHaveBeenCalled();
  });

  it("falls back to the copy command when navigator.clipboard is undefined", async () => {
    const { doc, areas } = fakeDocument(true);
    await expect(copyTextToClipboard("http", {}, doc)).resolves.toBeUndefined();
    expect(areas[0].value).toBe("http");
  });

  it("rejects with ClipboardUnavailableError when navigator.clipboard is undefined and the command is refused", async () => {
    await expect(copyTextToClipboard("http", {}, fakeDocument(false).doc)).rejects.toBeInstanceOf(ClipboardUnavailableError);
    await expect(copyTextToClipboard("http", undefined, undefined)).rejects.toBeInstanceOf(ClipboardUnavailableError);
  });

  it("passes an async clipboard refusal through untouched", async () => {
    const denied = new Error("Document is not focused.");
    denied.name = "NotAllowedError";
    const { doc } = fakeDocument(true);
    await expect(copyTextToClipboard("x", { clipboard: { writeText: () => Promise.reject(denied) } }, doc))
      .rejects.toBe(denied);
    expect(doc.execCommand).not.toHaveBeenCalled();
  });
});
