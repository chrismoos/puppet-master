export const SCROLLBAR_IDLE_MS = 750;

export interface TransientScrollbarOptions {
  /** Toggled on the host while the scrollbar should be visible. */
  activeClass: string;
  /** Added for the lifetime of the wiring so stylesheets can scope the hidden state. */
  hostClass?: string;
  /** Reports whether a pointer event is over the scrollbar. */
  onScrollbar: (event: PointerEvent) => boolean;
  /** Reveals on hover too, for a native bar that offers no DOM to aim at once it fades out. */
  revealOnHover?: boolean;
  /** Host events that count as user scrolling. */
  revealOn?: readonly string[];
}

const SCROLL_KEYS = new Set(["PageUp", "PageDown", "Home", "End"]);

/**
 * Reveals a scrollbar only around explicit user scrolling, and holds it up for
 * the whole of a thumb drag — the idle timer must not retract the bar out from
 * under the pointer dragging it.
 */
export function wireTransientScrollbar(
  host: HTMLElement,
  options: TransientScrollbarOptions,
): () => void {
  let hideTimer = 0;
  let dragging = false;

  if (options.hostClass) host.classList.add(options.hostClass);

  const schedule = () => {
    window.clearTimeout(hideTimer);
    hideTimer = 0;
    if (dragging) return;
    hideTimer = window.setTimeout(() => {
      host.classList.remove(options.activeClass);
      hideTimer = 0;
    }, SCROLLBAR_IDLE_MS);
  };

  const reveal = () => {
    host.classList.add(options.activeClass);
    schedule();
  };

  const onKeyDown = (event: KeyboardEvent) => {
    if (SCROLL_KEYS.has(event.key)) reveal();
  };

  const onPointerDown = (event: PointerEvent) => {
    if (!options.onScrollbar(event)) return;
    dragging = true;
    reveal();
  };

  const onPointerMove = (event: PointerEvent) => {
    if (options.onScrollbar(event)) reveal();
  };

  const endDrag = () => {
    if (!dragging) return;
    dragging = false;
    schedule();
  };

  const revealEvents = options.revealOn ?? ["wheel", "touchmove"];
  for (const name of revealEvents) {
    host.addEventListener(name, reveal, { capture: true, passive: true });
  }
  host.addEventListener("keydown", onKeyDown, true);
  host.addEventListener("pointerdown", onPointerDown, true);
  if (options.revealOnHover) {
    host.addEventListener("pointermove", onPointerMove, { capture: true, passive: true });
  }
  // A release outside the host still ends the drag, so a pointer that leaves the
  // element or a window that loses focus mid-drag cannot strand the bar revealed.
  window.addEventListener("pointerup", endDrag, true);
  window.addEventListener("pointercancel", endDrag, true);
  window.addEventListener("blur", endDrag);

  return () => {
    window.clearTimeout(hideTimer);
    for (const name of revealEvents) host.removeEventListener(name, reveal, true);
    host.removeEventListener("keydown", onKeyDown, true);
    host.removeEventListener("pointerdown", onPointerDown, true);
    host.removeEventListener("pointermove", onPointerMove, true);
    window.removeEventListener("pointerup", endDrag, true);
    window.removeEventListener("pointercancel", endDrag, true);
    window.removeEventListener("blur", endDrag);
    host.classList.remove(options.activeClass);
    if (options.hostClass) host.classList.remove(options.hostClass);
  };
}

/**
 * A native scrollbar has no DOM to hit-test, so a pointer is attributed to it by
 * landing past the content box, which `clientWidth` excludes.
 */
export function overNativeVerticalScrollbar(host: HTMLElement, event: PointerEvent): boolean {
  if (host.scrollHeight <= host.clientHeight) return false;
  return event.clientX - host.getBoundingClientRect().left > host.clientWidth;
}
