import { wireTransientScrollbar } from "../transientScrollbar";

/** Reveals xterm's custom scrollbar only around explicit user scrolling. */
export function wireTransientTerminalScrollbar(host: HTMLElement): () => void {
  return wireTransientScrollbar(host, {
    hostClass: "terminal-scrollbar-shell",
    activeClass: "is-terminal-scrollbar-active",
    onScrollbar: (event) =>
      event.target instanceof Element && event.target.closest(".scrollbar.vertical") !== null,
  });
}
