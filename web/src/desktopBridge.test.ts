// @vitest-environment jsdom
import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { beforeAll, describe, expect, it } from "vitest";

const BRIDGE = resolve(__dirname, "../../apps/desktop/src-tauri/src/bridge.js");
const opened: string[] = [];

describe("the desktop app's window.open", () => {
  beforeAll(() => {
    (window as unknown as { __TAURI_INTERNALS__: unknown }).__TAURI_INTERNALS__ = {
      invoke: (command: string, args: { url?: string }) => {
        if (command === "open_window" && args.url) opened.push(args.url);
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
});
