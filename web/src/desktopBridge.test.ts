// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { beforeAll, describe, expect, it } from "vitest";

const BRIDGE = resolve(__dirname, "../../apps/desktop/src-tauri/src/bridge.js");
const opened: string[] = [];
const zooms: number[] = [];

describe("the desktop app's window.open", () => {
  beforeAll(() => {
    (window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {
      invoke: (command: string, args: { url?: string; step?: number }) => {
        if (command === "open_window" && args.url?.includes("refused")) return Promise.reject("macOS found no app to open it");
        if (command === "open_window" && args.url) opened.push(args.url);
        if (command === "zoom") zooms.push((args as { step: number }).step);
        return Promise.resolve(false);
      },
      metadata: {},
    };
    new Function(readFileSync(BRIDGE, "utf8"))();
  });

  it("opens a link passed straight to it", () => {
    opened.length = 0;
    window.open("https://example.com/a", "_blank", "noopener,noreferrer");
    expect(opened).toEqual(["https://example.com/a"]);
  });

  it("opens a blank tab's link once the caller points it somewhere", () => {
    opened.length = 0;
    const tab = window.open("", "_blank")!;
    expect(opened).toEqual([]);
    tab.opener = null;
    tab.location.href = "https://share.example.com/forward";
    const other = window.open("", "_blank")!;
    (other as unknown as { location: string }).location = "/relative";
    expect(opened).toEqual(["https://share.example.com/forward", new URL("/relative", location.href).href]);
  });

  it("shows a link that could not be opened as a link error", async () => {
    const errors: string[] = [];
    window.addEventListener("pm-terminal-link-error", (event) => errors.push((event as CustomEvent<string>).detail));
    window.open("https://refused.example.com/", "_blank");
    await new Promise((resolve) => setTimeout(resolve, 0));
    expect(errors).toEqual(["Could not open https://refused.example.com/: macOS found no app to open it"]);
  });

  it("zooms on ⌘+, ⌘− and ⌘0 and leaves other keys to the page", () => {
    zooms.length = 0;
    const press = (key: string, meta = true) => {
      const event = new KeyboardEvent("keydown", { key, metaKey: meta, bubbles: true, cancelable: true });
      document.body.dispatchEvent(event);
      return event.defaultPrevented;
    };
    expect([press("="), press("+"), press("-"), press("0")]).toEqual([true, true, true, true]);
    expect(press("k")).toBe(false);
    expect(press("=", false)).toBe(false);
    expect(zooms).toEqual([1, 1, -1, 0]);
  });
});
