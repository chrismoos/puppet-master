import {
  ClipboardUnavailableError,
  writeClipboardText,
  type NavigatorClipboardHost,
} from "@puppet-master/client-core/ws/clipboard";

export interface CopyCommandDocument {
  body: { appendChild(node: unknown): unknown };
  createElement(tag: "textarea"): CopyCommandTextArea;
  execCommand?(command: "copy"): boolean;
}

export interface CopyCommandTextArea {
  value: string;
  setAttribute(name: string, value: string): void;
  style: { position: string; left: string; top: string; opacity: string };
  select(): void;
  remove(): void;
}

/**
 * The legacy copy command, the only clipboard write a plain-HTTP origin
 * has. It succeeds only inside a user gesture and reports refusal by
 * returning false.
 */
export function execCommandCopy(doc: CopyCommandDocument | undefined, text: string): boolean {
  if (!doc || typeof doc.execCommand !== "function") return false;
  const area = doc.createElement("textarea");
  area.value = text;
  area.setAttribute("readonly", "");
  area.setAttribute("aria-hidden", "true");
  area.style.position = "fixed";
  area.style.left = "-9999px";
  area.style.top = "0";
  area.style.opacity = "0";
  doc.body.appendChild(area);
  try {
    area.select();
    return doc.execCommand("copy") === true;
  } catch {
    return false;
  } finally {
    area.remove();
  }
}

/**
 * Writes through the async clipboard API when the origin has one, and
 * through the legacy copy command otherwise. Rejects with
 * ClipboardUnavailableError when neither path accepted the text.
 */
export async function copyTextToClipboard(
  text: string,
  host: NavigatorClipboardHost | undefined = (globalThis as { navigator?: NavigatorClipboardHost }).navigator,
  doc: CopyCommandDocument | undefined = (globalThis as { document?: CopyCommandDocument }).document,
): Promise<void> {
  try {
    await writeClipboardText(text, host);
  } catch (error) {
    if (!(error instanceof ClipboardUnavailableError)) throw error;
    if (!execCommandCopy(doc, text)) throw error;
  }
}
