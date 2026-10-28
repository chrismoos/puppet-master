export const SELECTION_MULTI_CLICK_INTERVAL_MS = 500;

export function isMacPlatform(platform = navigator.platform): boolean {
  return /mac|iphone|ipad|ipod/i.test(platform);
}

export interface WrappedTerminalLinkLine {
  readonly text: string;
  readonly segments: readonly {
    readonly bufferLineNumber: number;
    readonly startIndex: number;
    readonly endIndex: number;
  }[];
}

interface LinkLine {
  readonly isWrapped: boolean;
  translateToString(trimRight?: boolean): string;
}

export function collectWrappedTerminalLinkLine(
  bufferLineNumber: number,
  getRow: (bufferLineNumber: number) => LinkLine | null,
): WrappedTerminalLinkLine | null {
  let first = bufferLineNumber;
  while (first > 1 && getRow(first)?.isWrapped) first -= 1;
  const segments: WrappedTerminalLinkLine["segments"][number][] = [];
  let text = "";
  for (let line = first; ; line += 1) {
    const row = getRow(line);
    if (!row) break;
    const value = row.translateToString(true);
    segments.push({ bufferLineNumber: line, startIndex: text.length, endIndex: text.length + value.length });
    text += value;
    if (!getRow(line + 1)?.isWrapped) break;
  }
  return segments.length === 0 ? null : { text, segments };
}

export interface TerminalLinkTextMatch {
  readonly text: string;
  readonly start: number;
  readonly end: number;
}

export function extractTerminalLinks(text: string): TerminalLinkTextMatch[] {
  const links: TerminalLinkTextMatch[] = [];
  const expression = /(?:https?:\/\/[^\s<>"'`\u0000-\u001f\u007f]+|pm:(?:item\/\d+(?:\/\d+)?|session\/\d+))/giu;
  for (const match of text.matchAll(expression)) {
    const value = match[0].replace(/[.,;:!?]+$/, "");
    if (!value) continue;
    const start = match.index;
    links.push({ text: value, start, end: start + value.length });
  }
  return links;
}

export function observeResize(element: Element, callback: () => void): () => void {
  const observer = new ResizeObserver(callback);
  observer.observe(element);
  return () => observer.disconnect();
}
