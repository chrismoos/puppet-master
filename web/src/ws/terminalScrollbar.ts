import { wireTransientScrollbar } from "../transientScrollbar";

export function wireTransientTerminalScrollbar(host: HTMLElement): () => void {
  return wireTransientScrollbar(host, {
    hostClass: "terminal-scrollbar-shell",
    activeClass: "is-terminal-scrollbar-active",
    onScrollbar: (event) =>
      event.target instanceof Element && event.target.closest(".ghostty-scrollbar") !== null,
  });
}
