import type { GhosttyColor, GhosttySnapshot, GhosttyTheme } from "./core";
import type { GhosttyCellMetrics } from "./renderer";
import { GhosttyTerminalSurface, type GhosttyTerminalSurfaceOptions } from "./surface";
import { OscObserver } from "./oscObserver";

export interface Disposable {
  dispose(): void;
}

export type GhosttyRendererIssue = "unavailable";

export interface GhosttyViewport {
  type: string;
  baseY: number;
  viewportY: number;
  length: number;
}

type OscHandler = (data: string) => boolean;
type DeferredOperation = (surface: GhosttyTerminalSurface) => void;

export class GhosttyTerminalAdapter {
  readonly element: HTMLDivElement;
  readonly fontFamily: string;
  readonly parser = {
    registerOscHandler: (id: number, listener: OscHandler): Disposable => {
      const listeners = this.oscHandlers.get(id) ?? new Set<OscHandler>();
      listeners.add(listener);
      this.oscHandlers.set(id, listeners);
      return { dispose: () => listeners.delete(listener) };
    },
  };
  readonly options: { theme: GhosttyTheme };
  readonly modes: {
    readonly synchronizedOutputMode: boolean;
    readonly mouseTrackingMode: "none" | "x10" | "vt200" | "drag" | "any";
  };
  readonly buffer: { readonly active: GhosttyViewport };

  private surface: GhosttyTerminalSurface | null = null;
  private disposed = false;
  private readonly pendingOperations: DeferredOperation[] = [];
  private readonly dataListeners = new Set<(value: string | Uint8Array) => void>();
  private readonly scrollListeners = new Set<() => void>();
  private readonly selectionCompleteListeners = new Set<() => void>();
  private readonly keyHandlers = new Set<(event: KeyboardEvent) => boolean>();
  private readonly oscHandlers = new Map<number, Set<OscHandler>>();
  private readonly renderListeners = new Set<() => void>();
  private readonly scanner = new OscObserver((payload) => this.receiveOsc(payload));
  private currentCols = 80;
  private currentRows = 24;
  private visible = true;
  private replaying = false;
  private focusRequested = false;
  private focusRequestElement: Element | null = null;

  constructor(
    private readonly host: HTMLDivElement,
    theme: GhosttyTheme,
    private readonly onResize: (cols: number, rows: number) => void,
    private readonly onError: (error: unknown) => void,
    onReady: () => void,
    private readonly onLinkActivate: NonNullable<GhosttyTerminalSurfaceOptions["onLinkActivate"]>,
    private readonly canActivateLink: NonNullable<GhosttyTerminalSurfaceOptions["canActivateLink"]>,
    private readonly font: GhosttyTerminalSurfaceOptions["font"] = undefined,
  ) {
    this.element = host;
    this.fontFamily = font?.family ?? '"JetBrains Mono Variable", monospace';
    this.options = { theme };
    const adapter = this;
    this.modes = Object.defineProperties({}, {
      synchronizedOutputMode: { get: () => adapter.surface?.synchronizedOutputMode() ?? false },
      mouseTrackingMode: { get: () => adapter.surface?.mouseTrackingMode() ?? "none" },
    }) as typeof this.modes;
    this.buffer = Object.defineProperties({}, {
      active: {
        get: () => adapter.surface?.viewportState()
          ?? { type: "normal", baseY: 0, viewportY: 0, length: 0 },
      },
    }) as typeof this.buffer;
    void GhosttyTerminalSurface.create(host, {
      theme,
      ...(this.font ? { font: this.font } : {}),
      visible: this.visible,
      onData: (data) => this.emitData(data),
      onResize: (cols, rows) => {
        if (this.disposed) return;
        this.currentCols = cols;
        this.currentRows = rows;
        this.onResize(cols, rows);
      },
      onViewportChange: () => {
        for (const listener of this.scrollListeners) listener();
      },
      onSelectionComplete: () => {
        for (const listener of this.selectionCompleteListeners) listener();
      },
      onRender: () => {
        for (const listener of this.renderListeners) listener();
      },
      beforeKey: (event) => [...this.keyHandlers].every((handler) => handler(event)),
      onLinkActivate: (text, event) => this.onLinkActivate(text, event),
      canActivateLink: (text) => this.canActivateLink(text),
      onContextMenu: (event) => event.preventDefault(),
    }).then((surface) => {
      if (this.disposed) {
        surface.dispose();
        return;
      }
      this.surface = surface;
      this.currentCols = surface.cols;
      this.currentRows = surface.rows;
      surface.setTheme(this.options.theme);
      surface.setVisible(this.visible);
      for (const operation of this.pendingOperations.splice(0)) operation(surface);
      if (this.focusRequested && this.visible &&
          (host.ownerDocument?.activeElement ?? null) === this.focusRequestElement) surface.focus();
      onReady();
    }).catch((error: unknown) => {
      if (!this.disposed) this.onError(error);
    });
  }

  get cols(): number { return this.surface?.cols ?? this.currentCols; }
  get rows(): number { return this.surface?.rows ?? this.currentRows; }
  get cellSize(): { cols: number; rows: number } | null { return this.surface?.measuredGrid() ?? null; }
  get cellMetrics(): Pick<GhosttyCellMetrics, "width" | "height"> | null {
    return this.surface?.measuredCellSize() ?? null;
  }
  get ready(): boolean { return this.surface !== null; }
  get selection(): string { return this.surface?.getSelection() ?? ""; }
  renderSnapshot(): GhosttySnapshot | null { return this.surface?.renderSnapshot() ?? null; }

  onData(listener: (value: string | Uint8Array) => void): Disposable {
    this.dataListeners.add(listener);
    return { dispose: () => this.dataListeners.delete(listener) };
  }

  onScroll(listener: () => void): Disposable {
    this.scrollListeners.add(listener);
    return { dispose: () => this.scrollListeners.delete(listener) };
  }

  onSelectionComplete(listener: () => void): Disposable {
    this.selectionCompleteListeners.add(listener);
    return { dispose: () => this.selectionCompleteListeners.delete(listener) };
  }

  onRender(listener: () => void): Disposable {
    this.renderListeners.add(listener);
    return { dispose: () => this.renderListeners.delete(listener) };
  }

  attachCustomKeyEventHandler(listener: (event: KeyboardEvent) => boolean): void {
    this.keyHandlers.add(listener);
  }

  write(data: string | Uint8Array, callback?: () => void): void {
    this.enqueue((surface) => {
      this.scanner.write(data);
      surface.write(data, callback);
    });
  }

  resetAndWrite(data: string | Uint8Array, callback?: () => void): void {
    this.enqueue((surface) => {
      this.scanner.reset();
      this.scanner.write(data);
      surface.resetAndWrite(data, callback);
    });
  }

  reset(): void {
    this.enqueue((surface) => {
      this.scanner.reset();
      surface.reset();
    });
  }
  setReplaying(replaying: boolean): void {
    this.enqueue(() => { this.replaying = replaying; });
  }

  resize(cols: number, rows: number): void {
    this.currentCols = cols;
    this.currentRows = rows;
    this.enqueue((surface) => surface.resize(cols, rows));
  }

  fit(): boolean {
    const current = this.surface;
    if (!current) return false;
    const before = { cols: current.cols, rows: current.rows };
    const changed = current.fit();
    return changed || before.cols !== current.cols || before.rows !== current.rows;
  }

  focus(): void {
    this.focusRequested = true;
    this.focusRequestElement = this.host.ownerDocument?.activeElement ?? null;
    this.surface?.focus();
  }

  setVisible(visible: boolean): void {
    this.visible = visible;
    this.surface?.setVisible(visible);
  }

  refresh(_start: number, _end: number): void {
    this.surface?.refresh();
  }

  scrollToBottom(): void { this.surface?.scrollToBottom(); }
  scrollToLine(line: number): void { this.surface?.scrollToLine(line); }

  getSelection(): string { return this.surface?.getSelection() ?? ""; }
  hasSelection(): boolean { return this.surface?.hasSelection() ?? false; }
  clearSelection(): void { this.surface?.clearSelection(); }

  setTheme(theme: GhosttyTheme): void {
    this.options.theme = theme;
    this.surface?.setTheme(theme);
  }

  setActive(active: boolean): void { this.setVisible(active); }

  dispose(): void {
    this.disposed = true;
    this.surface?.dispose();
    this.surface = null;
    this.oscHandlers.clear();
    this.dataListeners.clear();
    this.scrollListeners.clear();
    this.selectionCompleteListeners.clear();
    this.keyHandlers.clear();
    this.renderListeners.clear();
    this.pendingOperations.length = 0;
  }

  private enqueue(operation: DeferredOperation): void {
    if (this.disposed) return;
    if (this.surface) operation(this.surface);
    else this.pendingOperations.push(operation);
  }

  private emitData(data: string | Uint8Array): void {
    for (const listener of this.dataListeners) listener(data);
  }

  private receiveOsc(payload: string): void {
    const separator = payload.indexOf(";");
    if (separator < 0) return;
    const id = Number(payload.slice(0, separator));
    if (this.replaying && id === 52) return;
    for (const listener of this.oscHandlers.get(id) ?? []) {
      if (listener(payload.slice(separator + 1))) return;
    }
  }
}

export function ghosttyColorToCss(color: GhosttyColor): string {
  return `#${[color.r, color.g, color.b].map((channel) => channel.toString(16).padStart(2, "0")).join("")}`;
}
