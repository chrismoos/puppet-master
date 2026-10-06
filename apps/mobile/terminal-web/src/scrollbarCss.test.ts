// The resting terminal must show no scrollbar. xterm renders its own DOM
// scrollbar (.xterm-scrollable-element > .scrollbar), so these tests pin the
// stylesheet state that governs that real element: the selector must still
// be the one xterm ships, and the template's hide rule must come after
// xterm's stylesheet so it wins the cascade at equal specificity.

import { readFileSync } from "node:fs";
import { createRequire } from "node:module";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

import { describe, expect, it } from "vitest";

const here = dirname(fileURLToPath(import.meta.url));
const require = createRequire(import.meta.url);

const template = readFileSync(join(here, "..", "index.html"), "utf8");
const xtermCss = readFileSync(require.resolve("@xterm/xterm/css/xterm.css"), "utf8");

const SCROLLBAR_SELECTOR = ".xterm-scrollable-element > .scrollbar";

function rule(css: string, selector: string): string | null {
  const start = css.indexOf(`${selector} {`);
  if (start < 0) return null;
  const open = css.indexOf("{", start);
  const close = css.indexOf("}", open);
  return css.slice(open + 1, close);
}

describe("terminal scrollbar css", () => {
  it("targets the scrollbar element xterm actually renders", () => {
    expect(xtermCss).toContain(SCROLLBAR_SELECTOR);
  });

  it("hides xterm's scrollbar element entirely", () => {
    const body = rule(template, `.xterm ${SCROLLBAR_SELECTOR}`);
    expect(body).not.toBeNull();
    expect(body).toContain("display: none");
  });

  it("places the hide rule after xterm's stylesheet so it wins the cascade", () => {
    const cssSlot = template.indexOf("__XTERM_CSS__");
    const hideRule = template.indexOf(`.xterm ${SCROLLBAR_SELECTOR} {`);
    expect(cssSlot).toBeGreaterThan(-1);
    expect(hideRule).toBeGreaterThan(cssSlot);
  });

  it("keeps native viewport scrollbars hidden", () => {
    const viewport = rule(template, ".xterm .xterm-viewport");
    expect(viewport).toContain("scrollbar-width: none");
    const webkit = rule(template, ".xterm .xterm-viewport::-webkit-scrollbar");
    expect(webkit).toContain("display: none");
  });

  it("rests the overlay indicator invisible and untouchable", () => {
    const overlay = rule(template, "#scrollbar");
    expect(overlay).not.toBeNull();
    expect(overlay).toContain("opacity: 0");
    expect(overlay).toContain("pointer-events: none");
  });
});
