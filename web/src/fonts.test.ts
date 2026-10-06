import { describe, expect, it, vi } from "vitest";
import { PRELOADED_FONT_SPECIFIERS, preloadTerminalFonts } from "./fonts";

const fontsWith = (load: (specifier: string) => Promise<unknown>) =>
  ({ load: vi.fn(load) }) as unknown as FontFaceSet & { load: ReturnType<typeof vi.fn> };

describe("terminal font preloading", () => {
  it("preloads an italic face alongside the upright one", () => {
    const styles = PRELOADED_FONT_SPECIFIERS.map((specifier) =>
      specifier.startsWith("italic ") ? "italic" : "normal",
    );
    expect(styles).toContain("italic");
    expect(styles).toContain("normal");
    for (const specifier of PRELOADED_FONT_SPECIFIERS) {
      expect(specifier).toContain('"JetBrains Mono Variable"');
    }
  });

  it("loads every specifier before the app renders", async () => {
    const fonts = fontsWith(() => Promise.resolve());
    await preloadTerminalFonts(fonts);
    expect(fonts.load.mock.calls.map(([specifier]) => specifier)).toEqual(
      PRELOADED_FONT_SPECIFIERS,
    );
  });

  it("still settles when one face cannot be fetched", async () => {
    const fonts = fontsWith((specifier) =>
      specifier.startsWith("italic ") ? Promise.reject(new Error("offline")) : Promise.resolve(),
    );
    await expect(preloadTerminalFonts(fonts)).resolves.toBeDefined();
    expect(fonts.load).toHaveBeenCalledTimes(PRELOADED_FONT_SPECIFIERS.length);
  });

  it("waits for a slow face instead of rendering as soon as one fails", async () => {
    const settled: string[] = [];
    let releaseSlow = () => {};
    const slow = new Promise<void>((resolve) => {
      releaseSlow = () => {
        settled.push("slow");
        resolve();
      };
    });
    const fonts = fontsWith((specifier) =>
      specifier.startsWith("italic ") ? slow : Promise.reject(new Error("offline")),
    );

    const preload = preloadTerminalFonts(fonts).then(() => settled.push("rendered"));
    await Promise.resolve();
    expect(settled).toEqual([]);

    releaseSlow();
    await preload;
    expect(settled).toEqual(["slow", "rendered"]);
  });
});
