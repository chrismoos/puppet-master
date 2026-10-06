import type { TerminalSize } from "@puppet-master/client-core/ws/pty";
import { TerminalSizePrompt } from "@puppet-master/client-core/ws/terminalSizePrompt";

export type TerminalSizeBanner = TerminalSizePrompt & { dispose(): void };

export function createTerminalSizePrompt(
  host: HTMLElement,
  measure: () => TerminalSize | null,
  resize: (size: TerminalSize) => void,
): TerminalSizeBanner {
  const banner = document.createElement("div");
  banner.className = "terminal-size-prompt";
  banner.hidden = true;
  banner.setAttribute("role", "status");
  banner.setAttribute("aria-live", "polite");
  const label = document.createElement("span");
  label.textContent = "This terminal is sized for another view. Resize it to fit this window?";
  const update = document.createElement("button");
  update.type = "button";
  update.textContent = "Update";
  const dismiss = document.createElement("button");
  dismiss.type = "button";
  dismiss.textContent = "Dismiss";
  const prompt = new TerminalSizePrompt(measure, (show) => { banner.hidden = !show; }, resize);
  update.addEventListener("click", () => prompt.update());
  dismiss.addEventListener("click", () => prompt.dismiss());
  banner.append(label, update, dismiss);
  host.appendChild(banner);
  return Object.assign(prompt, { dispose: () => { prompt.reset(); banner.remove(); } });
}
