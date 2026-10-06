import { Terminal, type ITheme } from "@xterm/xterm";
import { freshXtermTheme, type TerminalThemeController } from "../theme/controller";
import type { PmClient, PtyHandle } from "@puppet-master/client-core/ws/client";
import type { TerminalStreamStatus } from "@puppet-master/client-core/ws/terminalSocket";
import { writeTerminalFrame } from "./terminalWriter";
import { attachWebglRenderer, type RendererIssue, type WebglHandle } from "./webglBudget";
import { registerPmTerminalLinkProvider } from "./terminalLinks";
import { fitFullWidth, measureFit } from "./terminalFit";
import { createTerminalSizePrompt, type TerminalSizeBanner } from "./terminalSizePrompt";
import { wireViewerSize, type ViewerSizeController } from "@puppet-master/client-core/ws/terminalRepaint";
import { wireTransientTerminalScrollbar } from "./terminalScrollbar";
import { documentVisible, watchDocumentVisibility } from "./documentVisibility";
import { TerminalClipboard } from "./terminalClipboard";
import { registerForegroundCommandHandler, wireTerminalClipboard } from "./terminalHostWiring";
import {
  captureTerminalViewport,
  loadTerminalViewport,
  saveTerminalViewport,
  type TerminalViewportBookmark,
} from "./terminalViewport";

const SCROLLBACK_LINES = 5_000;
const FONT_SIZE_PX = 13;
const FAST_SCROLL_LINES = 8;
const MONO_FONT_STACK = '"JetBrains Mono Variable", ui-monospace, "SF Mono", Menlo, Consolas, "DejaVu Sans Mono", monospace';

export class WorkspaceTerminal {
  private terminal: Terminal;
  private webgl: WebglHandle | null = null;
  private handle: PtyHandle;
  private observer: ResizeObserver;
  private disposers: Array<() => void>;
  private resizeFrame = 0;
  private active = true;
  private viewer: ViewerSizeController;
  private viewport: TerminalViewportBookmark;
  private viewportRevision = 0;
  private pendingWrites = 0;
  private sizePrompt: TerminalSizeBanner;

  constructor(client: PmClient, host: HTMLElement, terminalId: bigint, themeController: TerminalThemeController, onIssue?: (reason: RendererIssue | null) => void, onStatus?: (status: TerminalStreamStatus) => void, onCommand?: (command: string) => void) {
    this.terminal = new Terminal({
      cursorBlink: true,
      macOptionClickForcesSelection: true,
      fontSize: FONT_SIZE_PX,
      fontFamily: MONO_FONT_STACK,
      scrollback: SCROLLBACK_LINES,
      fastScrollSensitivity: FAST_SCROLL_LINES,
      theme: themeController.currentXtermTheme(),
    });
    this.terminal.open(host);
    const viewportKey = `t:${terminalId}`;
    this.viewport = loadTerminalViewport(viewportKey) ?? captureTerminalViewport(this.terminal);
    const scrollbarDispose = wireTransientTerminalScrollbar(host);
    const pmLinks = registerPmTerminalLinkProvider(this.terminal, client, {
      kind: "terminal",
      id: terminalId.toString(),
    }, () => {
      this.viewportRevision += 1;
      this.viewport = captureTerminalViewport(this.terminal);
      saveTerminalViewport(viewportKey, this.viewport);
    });
    this.webgl = attachWebglRenderer(this.terminal, onIssue);

    fitFullWidth(this.terminal);
    this.handle = client.openTerminal(terminalId, measureFit(this.terminal));
    this.sizePrompt = createTerminalSizePrompt(host, () => measureFit(this.terminal), (size) => {
      if (!documentVisible(document)) return;
      this.handle.resize(size.cols, size.rows);
    });
    this.sizePrompt.reset();
    const initialFit = measureFit(this.terminal);
    if (initialFit) this.sizePrompt.requested(initialFit);
    const sizeEcho = this.handle.onViewerOwnership?.((ownership) => this.sizePrompt.observe(ownership)) ?? (() => {});
    const encoder = new TextEncoder();
    const data = this.terminal.onData((text) => this.handle.input(encoder.encode(text), text === "\r"));
    const binary = this.terminal.onBinary((chunk) => {
      const bytes = new Uint8Array(chunk.length);
      for (let index = 0; index < chunk.length; index += 1) bytes[index] = chunk.charCodeAt(index);
      this.handle.input(bytes);
    });
    const command = onCommand ? registerForegroundCommandHandler(this.terminal, onCommand) : null;
    const clipboard = new TerminalClipboard(document);
    host.appendChild(clipboard.el as HTMLElement);
    clipboard.attach();
    const clipboardDispose = wireTerminalClipboard(this.terminal, host, clipboard);
    this.handle.connect((frame) => {
      const viewport = this.viewport;
      const revision = this.viewportRevision;
      this.pendingWrites += 1;
      writeTerminalFrame(
        this.terminal,
        frame,
        viewport,
        () => {
          this.pendingWrites -= 1;
        },
        () => this.viewportRevision === revision,
      );
    });
    const scroll = this.terminal.onScroll(() => {
      if (this.pendingWrites > 0) return;
      this.viewport = captureTerminalViewport(this.terminal);
      saveTerminalViewport(viewportKey, this.viewport);
    });
    const rememberUserViewport = () => {
      this.viewportRevision += 1;
      this.viewport = captureTerminalViewport(this.terminal);
      saveTerminalViewport(viewportKey, this.viewport);
    };
    const rememberKeyboardViewport = (event: KeyboardEvent) => {
      if (!["PageUp", "PageDown", "Home", "End"].includes(event.key)) return;
      rememberUserViewport();
    };
    host.addEventListener("wheel", rememberUserViewport, { passive: true });
    host.addEventListener("pointerup", rememberUserViewport);
    host.addEventListener("keyup", rememberKeyboardViewport);
    const status = onStatus ? this.handle.onStatus(onStatus) : () => {};
    const terminal = this.terminal;
    const viewer = wireViewerSize({
      resize: (cols, rows) => {
        this.sizePrompt.requested({ cols, rows });
        this.handle.resize(cols, rows);
      },
      onStatus: (listener) => this.handle.onStatus(listener),
      ptySize: () => this.handle.ptySize(),
    }, {
      get cols() { return measureFit(terminal)?.cols ?? 0; },
      get rows() { return measureFit(terminal)?.rows ?? 0; },
    }, { canAssert: () => this.sizePrompt.needsClaim() });
    this.viewer = viewer;

    if (documentVisible(document)) viewer.opened();
    const visibility = watchDocumentVisibility(document, (visible) => {
      viewer.setVisible(visible && this.active);
    });
    this.observer = new ResizeObserver(() => this.scheduleResize());
    this.observer.observe(host);
    const unregisterTheme = themeController.register(this);
    this.disposers = [() => data.dispose(), () => binary.dispose(), () => scroll.dispose(), () => host.removeEventListener("wheel", rememberUserViewport), () => host.removeEventListener("pointerup", rememberUserViewport), () => host.removeEventListener("keyup", rememberKeyboardViewport), () => command?.dispose(), clipboardDispose, () => clipboard.dispose(), () => pmLinks.dispose(), scrollbarDispose, sizeEcho, status, visibility, () => viewer.dispose(), unregisterTheme];
    this.scheduleResize();
  }

  setActive(active: boolean): void {
    this.active = active;
    this.webgl?.setVisible(active);
    this.viewer.setVisible(active && documentVisible(document));
  }

  focus(): void {
    this.terminal.focus();
  }

  retry(): void {
    this.handle.retry();
  }

  /** Assigning the theme option repaints colors without touching the PTY,
   * terminal buffers, viewport, selection, focus, renderer, or geometry. */
  setTheme(theme: ITheme): void {
    this.terminal.options.theme = freshXtermTheme(theme);
  }

  dispose(): void {
    this.sizePrompt.dispose();
    this.observer.disconnect();
    cancelAnimationFrame(this.resizeFrame);
    for (const dispose of this.disposers) dispose();
    this.handle.close();
    this.webgl?.dispose();
    this.terminal.dispose();
  }

  private resize(): void {
    this.sizePrompt.localChanged();
    if (this.sizePrompt.blocked()) return;
    const fitChanged = fitFullWidth(this.terminal);
    // Compare against the daemon's PTY size echo so an unchanged size is
    // never re-sent, while a PTY resized by another viewer is corrected
    // even when this pane's own fit did not move.
    const ptySize = this.handle.ptySize();
    const matchesPty = ptySize !== null
      && ptySize.cols === this.terminal.cols
      && ptySize.rows === this.terminal.rows;
    if (matchesPty || (ptySize === null && !fitChanged)) return;
    this.sizePrompt.requested({ cols: this.terminal.cols, rows: this.terminal.rows });
    this.handle.resize(this.terminal.cols, this.terminal.rows);
  }

  private scheduleResize(): void {
    if (this.resizeFrame) return;
    this.resizeFrame = requestAnimationFrame(() => {
      this.resizeFrame = 0;
      this.resize();
    });
  }
}
