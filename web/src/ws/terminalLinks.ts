import type { IDisposable, ILink, ILinkProvider, Terminal } from "@xterm/xterm";
import { parsePmLink } from "@puppet-master/client-core/pmlink";
import { navigate, parseRoute } from "../router";
import { isReviewEntryRoute } from "../views/reviewWindow";
import { itemKey, type AppState } from "@puppet-master/client-core/state/reducer";
import type { PmClient } from "@puppet-master/client-core/ws/client";

export const TERMINAL_PM_LINK_ERROR_EVENT = "pm-terminal-link-error";

export type TerminalLinkSource =
  | { kind: "session"; id: string }
  | { kind: "terminal"; id: string };

type CellPosition = { x: number; y: number; endX: number };
type MouseTrackingMode = Terminal["modes"]["mouseTrackingMode"];

export interface TerminalLinkMatch {
  text: string;
  range: ILink["range"];
}

export type TerminalPmLinkMatch = TerminalLinkMatch;

export interface TerminalPmLinkResolution {
  path?: string;
  error?: string;
}

const PM_REFERENCE = /pm:(?:item\/\d+(?:\/\d+)?|session\/\d+)/g;
const HTTP_REFERENCE = /https?:\/\/[^\s<>"'`\u0000-\u001f\u007f]+/giu;
const IDENTIFIER_CHAR = /[A-Za-z0-9_]/;
const LINK_DRAG_THRESHOLD_PX = 4;
const LINK_HINT_CLASS = "terminal-pm-link-hint";
const TRAILING_URL_PUNCTUATION = /[.,;:!?]+$/;
const URL_BRACKET_PAIRS = [["(", ")"], ["[", "]"], ["{", "}"]] as const;

function sourceBucketId(state: AppState, source: TerminalLinkSource): string | null {
  const session = source.kind === "session"
    ? state.sessions.get(source.id)
    : (() => {
        const terminal = state.terminals.get(source.id);
        return terminal ? state.sessions.get(terminal.sessionId.toString()) : undefined;
      })();
  if (!session) return null;
  return state.projects.get(session.projectId.toString())?.bucketId.toString() ?? null;
}

function sessionBucketId(state: AppState, sessionId: string): string | null {
  const session = state.sessions.get(sessionId);
  if (!session) return null;
  return state.projects.get(session.projectId.toString())?.bucketId.toString() ?? null;
}

export function resolveTerminalPmLink(
  state: AppState,
  source: TerminalLinkSource,
  href: string,
): TerminalPmLinkResolution {
  if (!state.hydrated) return { error: "Puppet Master is still loading; try the link again." };
  const bucketId = sourceBucketId(state, source);
  if (bucketId === null) return { error: "This terminal is no longer available." };

  const link = parsePmLink(href);
  if (!link || (link.kind !== "item" && link.kind !== "legacyItem" && link.kind !== "session")) {
    return { error: `Unsupported Puppet Master link: ${href}` };
  }

  if (link.kind === "legacyItem") {
    return { error: `Legacy link pm:item/${link.legacyId} is unqualified; use pm:item/${bucketId}/<item>.` };
  }

  if (link.kind === "item") {
    if (link.bucketId !== bucketId) {
      return { error: `Item ${link.bucketId}/${link.id} is outside this terminal's bucket.` };
    }
    const item = state.items.get(itemKey(link.bucketId, link.id));
    if (!item) return { error: `Item ${link.id} is missing or inaccessible.` };
    if (item.bucketId.toString() !== bucketId) {
      return { error: `Item ${link.id} is outside this terminal's bucket.` };
    }
    return { path: `/bucket/${link.bucketId}/item/${link.id}` };
  }

  const targetBucketId = sessionBucketId(state, link.id);
  if (targetBucketId === null) return { error: `Session ${link.id} is missing or inaccessible.` };
  if (targetBucketId !== bucketId) {
    return { error: `Session ${link.id} is outside this terminal's bucket.` };
  }
  return { path: `/session/${link.id}` };
}

export function isTerminalLinkActivation(
  event: Pick<MouseEvent, "button" | "shiftKey" | "altKey" | "ctrlKey" | "metaKey">,
  mouseTrackingMode: MouseTrackingMode = "none",
  dragged = false,
): boolean {
  if (event.button !== 0 || dragged) return false;
  return mouseTrackingMode === "none" || event.shiftKey || event.altKey;
}

export const isTerminalPmLinkActivation = isTerminalLinkActivation;

class TerminalLinkGestureGuard {
  private start: { x: number; y: number; button: number } | null = null;
  private dragged = false;

  constructor(private element: HTMLElement) {
    element.addEventListener("mousedown", this.onMouseDown, true);
    element.addEventListener("mousemove", this.onMouseMove, true);
    element.addEventListener("mouseup", this.onMouseUp, true);
  }

  didDrag(): boolean {
    return this.dragged;
  }

  dispose(): void {
    this.element.removeEventListener("mousedown", this.onMouseDown, true);
    this.element.removeEventListener("mousemove", this.onMouseMove, true);
    this.element.removeEventListener("mouseup", this.onMouseUp, true);
  }

  private moved(clientX: number, clientY: number): boolean {
    return this.start !== null
      && Math.hypot(clientX - this.start.x, clientY - this.start.y) >= LINK_DRAG_THRESHOLD_PX;
  }

  private onMouseDown = (event: MouseEvent): void => {
    this.start = { x: event.clientX, y: event.clientY, button: event.button };
    this.dragged = false;
  };

  private onMouseMove = (event: MouseEvent): void => {
    if (this.moved(event.clientX, event.clientY)) this.dragged = true;
  };

  private onMouseUp = (event: MouseEvent): void => {
    if (!this.start) {
      this.dragged = true;
      return;
    }
    if (event.button !== this.start.button || this.moved(event.clientX, event.clientY)) this.dragged = true;
    const start = this.start;
    window.setTimeout(() => {
      if (this.start === start) {
        this.start = null;
        this.dragged = false;
      }
    }, 0);
  };
}

function logicalLine(terminal: Terminal, bufferLineNumber: number): { text: string; positions: CellPosition[] } | null {
  const buffer = terminal.buffer.active;
  let start = bufferLineNumber - 1;
  if (!buffer.getLine(start)) return null;
  while (start > 0 && buffer.getLine(start)?.isWrapped) start -= 1;

  let end = bufferLineNumber - 1;
  while (buffer.getLine(end + 1)?.isWrapped) end += 1;

  let text = "";
  const positions: CellPosition[] = [];
  for (let y = start; y <= end; y += 1) {
    const line = buffer.getLine(y);
    if (!line) break;
    for (let x = 0; x < terminal.cols; x += 1) {
      const cell = line.getCell(x);
      const width = cell?.getWidth() ?? 1;
      if (width === 0) continue;
      const chars = cell?.getChars() || " ";
      text += chars;
      for (let unit = 0; unit < chars.length; unit += 1) {
        positions.push({ x: x + 1, y: y + 1, endX: x + Math.max(width, 1) });
      }
    }
  }

  const trimmedLength = text.trimEnd().length;
  return { text: text.slice(0, trimmedLength), positions: positions.slice(0, trimmedLength) };
}

function hasValidBoundaries(text: string, start: number, end: number): boolean {
  const before = start > 0 ? text[start - 1] : "";
  const after = end < text.length ? text[end] : "";
  return (!before || !IDENTIFIER_CHAR.test(before))
    && (!after || (!IDENTIFIER_CHAR.test(after) && after !== "/"));
}

export function terminalPmLinksForLine(terminal: Terminal, bufferLineNumber: number): TerminalPmLinkMatch[] {
  const logical = logicalLine(terminal, bufferLineNumber);
  if (!logical) return [];

  const links: TerminalPmLinkMatch[] = [];
  for (const match of logical.text.matchAll(PM_REFERENCE)) {
    const start = match.index;
    const end = start + match[0].length;
    if (!hasValidBoundaries(logical.text, start, end)) continue;
    const first = logical.positions[start];
    const last = logical.positions[end - 1];
    if (!first || !last) continue;
    links.push({
      text: match[0],
      range: {
        start: { x: first.x, y: first.y },
        end: { x: last.endX, y: last.y },
      },
    });
  }
  return links;
}

function occurrences(text: string, character: string): number {
  return [...text].filter((value) => value === character).length;
}

function trimUrlCandidate(candidate: string): string {
  let trimmed = candidate.replace(TRAILING_URL_PUNCTUATION, "");
  let changed = true;
  while (changed) {
    changed = false;
    for (const [opening, closing] of URL_BRACKET_PAIRS) {
      if (trimmed.endsWith(closing) && occurrences(trimmed, closing) > occurrences(trimmed, opening)) {
        trimmed = trimmed.slice(0, -1).replace(TRAILING_URL_PUNCTUATION, "");
        changed = true;
      }
    }
  }
  return trimmed;
}

export function isSafeHttpUrl(href: string): boolean {
  if (/%(?![\dA-Fa-f]{2})/.test(href)) return false;
  try {
    const parsed = new URL(href);
    return (parsed.protocol === "http:" || parsed.protocol === "https:") && parsed.hostname.length > 0;
  } catch {
    return false;
  }
}

export function terminalHttpLinksForLine(terminal: Terminal, bufferLineNumber: number): TerminalLinkMatch[] {
  const logical = logicalLine(terminal, bufferLineNumber);
  if (!logical) return [];

  const links: TerminalLinkMatch[] = [];
  for (const match of logical.text.matchAll(HTTP_REFERENCE)) {
    const text = trimUrlCandidate(match[0]);
    const start = match.index;
    const end = start + text.length;
    const before = start > 0 ? logical.text[start - 1] : "";
    if (!text || (before && IDENTIFIER_CHAR.test(before)) || !isSafeHttpUrl(text)) continue;
    const first = logical.positions[start];
    const last = logical.positions[end - 1];
    if (!first || !last) continue;
    links.push({
      text,
      range: {
        start: { x: first.x, y: first.y },
        end: { x: last.endX, y: last.y },
      },
    });
  }
  return links;
}

function reportLinkError(message: string): void {
  window.dispatchEvent(new CustomEvent<string>(TERMINAL_PM_LINK_ERROR_EVENT, { detail: message }));
}

function terminalLinkHint(mouseTrackingMode: MouseTrackingMode, href: string): string {
  return mouseTrackingMode === "none"
    ? `Click to open ${href} (Command-click, Ctrl-click, and Shift-click also work)`
    : `Shift-click (Option-click on macOS) to open ${href} while terminal mouse tracking is active`;
}

function showTerminalLinkHint(terminal: Terminal, event: MouseEvent, href: string): void {
  const root = terminal.element;
  if (!root) return;
  root.querySelector(`.${LINK_HINT_CLASS}`)?.remove();
  const hint = document.createElement("div");
  hint.className = `${LINK_HINT_CLASS} xterm-hover`;
  hint.role = "tooltip";
  hint.textContent = terminalLinkHint(terminal.modes.mouseTrackingMode, href);
  const bounds = root.getBoundingClientRect();
  hint.style.left = `${event.clientX - bounds.left + 10}px`;
  hint.style.top = `${event.clientY - bounds.top + 14}px`;
  root.appendChild(hint);
}

function hideTerminalLinkHint(terminal: Terminal): void {
  terminal.element?.querySelector(`.${LINK_HINT_CLASS}`)?.remove();
}

export class PmTerminalLinkProvider implements ILinkProvider {
  constructor(
    private terminal: Terminal,
    private getState: () => AppState,
    private source: TerminalLinkSource,
    private go: (path: string) => void = navigate,
    private reportError: (message: string) => void = reportLinkError,
    private didDrag: () => boolean = () => false,
    private prepareNavigation: () => void = () => {},
  ) {}

  provideLinks(bufferLineNumber: number, callback: (links: ILink[] | undefined) => void): void {
    const links = terminalPmLinksForLine(this.terminal, bufferLineNumber).map<ILink>((match) => ({
      ...match,
      hover: (event, href) => showTerminalLinkHint(this.terminal, event, href),
      leave: () => hideTerminalLinkHint(this.terminal),
      dispose: () => hideTerminalLinkHint(this.terminal),
      activate: (event, href) => {
        if (!isTerminalLinkActivation(event, this.terminal.modes.mouseTrackingMode, this.didDrag())) return;
        event.preventDefault();
        // xterm owns the selection gesture that began on mousedown. Allow its
        // mouseup listeners to finish before a route change hides this layer;
        // stopping propagation here leaves selection/autoscroll latched.
        this.prepareNavigation();
        const resolution = resolveTerminalPmLink(this.getState(), this.source, href);
        const path = resolution.path;
        if (path) queueMicrotask(() => this.go(path));
        else if (resolution.error) this.reportError(resolution.error);
      },
    }));
    callback(links.length ? links : undefined);
  }
}

export function openTerminalHttpLink(href: string): void {
  const destination = new URL(href, location.href);
  if (destination.origin === location.origin && destination.pathname === location.pathname
    && isReviewEntryRoute(parseRoute(destination.hash))) {
    window.open(destination.href, "_blank");
  } else {
    window.open(href, "_blank", "noopener,noreferrer");
  }
}

export class HttpTerminalLinkProvider implements ILinkProvider {
  constructor(
    private terminal: Terminal,
    private open: (href: string) => void = openTerminalHttpLink,
    private didDrag: () => boolean = () => false,
    private prepareNavigation: () => void = () => {},
  ) {}

  provideLinks(bufferLineNumber: number, callback: (links: ILink[] | undefined) => void): void {
    const links = terminalHttpLinksForLine(this.terminal, bufferLineNumber).map<ILink>((match) => ({
      ...match,
      hover: (event, href) => showTerminalLinkHint(this.terminal, event, href),
      leave: () => hideTerminalLinkHint(this.terminal),
      dispose: () => hideTerminalLinkHint(this.terminal),
      activate: (event, href) => {
        if (!isTerminalLinkActivation(event, this.terminal.modes.mouseTrackingMode, this.didDrag())) return;
        if (!isSafeHttpUrl(href)) return;
        event.preventDefault();
        this.prepareNavigation();
        this.open(href);
      },
    }));
    callback(links.length ? links : undefined);
  }
}

export function registerPmTerminalLinkProvider(
  terminal: Terminal,
  client: Pick<PmClient, "getState">,
  source: TerminalLinkSource,
  prepareNavigation: () => void = () => {},
): IDisposable {
  if (!terminal.element) throw new Error("xterm must be opened before registering PM links");
  const gestures = new TerminalLinkGestureGuard(terminal.element);
  const pmProvider = terminal.registerLinkProvider(new PmTerminalLinkProvider(
    terminal,
    client.getState,
    source,
    navigate,
    reportLinkError,
    () => gestures.didDrag(),
    prepareNavigation,
  ));
  const httpProvider = terminal.registerLinkProvider(new HttpTerminalLinkProvider(
    terminal,
    undefined,
    () => gestures.didDrag(),
    prepareNavigation,
  ));
  return {
    dispose: () => {
      pmProvider.dispose();
      httpProvider.dispose();
      gestures.dispose();
      hideTerminalLinkHint(terminal);
    },
  };
}
