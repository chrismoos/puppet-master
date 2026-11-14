// @vitest-environment jsdom
import { afterEach, describe, expect, it, vi } from "vitest";
import { dispatchContextMenu, overlayRegions } from "./nativeTerminal";

function at(element: HTMLElement, left: number, top: number, width: number, height: number): HTMLElement {
  element.getBoundingClientRect = () => new DOMRect(left, top, width, height);
  return element;
}

function add(html: string): HTMLElement {
  const holder = document.createElement("div");
  holder.innerHTML = html;
  const element = holder.firstElementChild as HTMLElement;
  document.body.append(element);
  return element;
}

describe("page content over a native terminal", () => {
  afterEach(() => {
    document.body.innerHTML = "";
    vi.restoreAllMocks();
  });

  it("lets menus, dialogs and alert cards show through, clipped to the terminal", () => {
    const host = at(add(`<div class="host"></div>`), 100, 100, 400, 300);
    at(add(`<div class="popover-menu"></div>`), 450, 150, 100, 80);
    at(add(`<div role="dialog"></div>`), 200, 200, 50, 50);
    at(add(`<div class="alert-card"></div>`), 600, 10, 200, 40);
    at(add(`<div class="alert-card"></div>`), 480, 120, 60, 40);
    expect(overlayRegions(host)).toEqual({
      holes: [
        { x: 450, y: 150, width: 50, height: 80 },
        { x: 200, y: 200, width: 50, height: 50 },
        { x: 480, y: 120, width: 20, height: 40 },
      ],
      passthrough: [],
    });
  });

  it("gives the hover header the mouse while closed and shows it once open", () => {
    const host = at(add(`<div class="host"></div>`), 0, 0, 400, 300);
    const grip = at(add(`<div class="ap-agent-grip"></div>`), 0, 0, 400, 6);
    expect(overlayRegions(host)).toEqual({ holes: [], passthrough: [{ x: 0, y: 0, width: 400, height: 6 }] });
    grip.classList.add("is-moving");
    expect(overlayRegions(host)).toEqual({ holes: [{ x: 0, y: 0, width: 400, height: 6 }], passthrough: [] });
  });

  it("hands a right click to the page element under it", () => {
    const host = add(`<div class="host"><span class="inner"></span></div>`);
    const inner = host.querySelector(".inner")!;
    document.elementFromPoint = () => inner;
    const seen = vi.fn((event: MouseEvent) => [event.clientX, event.clientY, event.button]);
    host.addEventListener("contextmenu", (event) => seen(event));
    dispatchContextMenu(120, 80, host);
    expect(seen.mock.results[0]?.value).toEqual([120, 80, 2]);
  });
});
