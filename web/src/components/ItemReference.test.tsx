import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, describe, expect, it, vi } from "vitest";
import { copyItemReference, ItemReference } from "./ItemReference";

describe("ItemReference", () => {
  afterEach(() => vi.unstubAllGlobals());

  it("renders an accessible compact copy control beside the item permalink", () => {
    const markup = renderToStaticMarkup(createElement(ItemReference, {
      bucketId: "7",
      id: "9007199254740993",
      onOpen: () => {},
    }));

    expect(markup).toContain('href="#/bucket/7/item/9007199254740993"');
    expect(markup).toContain("pm:item/7/9007199254740993");
    expect(markup).toContain('type="button"');
    expect(markup).toContain('aria-label="Copy item reference pm:item/7/9007199254740993"');
    expect(markup).toContain('aria-hidden="true"');
  });

  it("copies exactly the internal item reference", async () => {
    const writeText = vi.fn().mockResolvedValue(undefined);
    vi.stubGlobal("navigator", { clipboard: { writeText } });

    await expect(copyItemReference("pm:item/4/118")).resolves.toBe("copied");
    expect(writeText).toHaveBeenCalledOnce();
    expect(writeText).toHaveBeenCalledWith("pm:item/4/118");
  });

  it("reports clipboard failures", async () => {
    vi.stubGlobal("navigator", { clipboard: { writeText: vi.fn().mockRejectedValue(new Error("denied")) } });

    await expect(copyItemReference("pm:item/4/118")).resolves.toBe("failed");
  });

  it("falls back to the copy command when navigator.clipboard is undefined", async () => {
    vi.stubGlobal("navigator", {});
    const execCommand = vi.fn(() => true);
    vi.stubGlobal("document", {
      body: { appendChild: () => {} },
      createElement: () => ({ value: "", setAttribute: () => {}, style: {}, select: () => {}, remove: () => {} }),
      execCommand,
    });

    await expect(copyItemReference("pm:item/4/118")).resolves.toBe("copied");
    expect(execCommand).toHaveBeenCalledWith("copy");
  });

  it("reports a failure instead of throwing when no clipboard path exists", async () => {
    vi.stubGlobal("navigator", {});
    vi.stubGlobal("document", undefined);

    await expect(copyItemReference("pm:item/4/118")).resolves.toBe("failed");
  });
});
