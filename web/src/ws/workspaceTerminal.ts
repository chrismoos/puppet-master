import type { TerminalThemeController } from "../theme/controller";
import type { GhosttyTheme } from "./ghostty/core";
import { GhosttyTerminalAdapter, type GhosttyRendererIssue } from "./ghostty/terminalAdapter";
import type { PmClient, PtyHandle } from "@puppet-master/client-core/ws/client";
import type { TerminalSize } from "@puppet-master/client-core/ws/pty";
import type { TerminalStreamStatus } from "@puppet-master/client-core/ws/terminalSocket";
import { applyEchoedPtySize } from "@puppet-master/client-core/ws/terminalResize";
import { writeTerminalFrame } from "./terminalWriter";
import { activateTerminalLink, isSafeHttpUrl } from "./terminalLinks";
import { fitFullWidth, measureFit } from "./terminalFit";
import { workspaceInputDisposition, workspaceInputReady, workspacePtyEchoDisposition, workspaceResizeRequest } from "./workspaceTerminalSizing";
import { holdInput, type HeldInput } from "./terminalLayer";
import { createTerminalSizePrompt, type TerminalSizeBanner } from "./terminalSizePrompt";
import { wireViewerSize, type ViewerSizeController } from "@puppet-master/client-core/ws/terminalRepaint";
import { documentVisible, watchDocumentVisibility } from "./documentVisibility";
import { TerminalClipboard } from "./terminalClipboard";
import { registerForegroundCommandHandler, wireTerminalClipboard } from "./terminalHostWiring";
import {
  captureTerminalViewport,
  loadTerminalViewport,
  saveTerminalViewport,
  type TerminalViewportBookmark,
} from "./terminalViewport";

const FONT_SIZE_PX = 13;
const DEFAULT_COLS = 80;
const DEFAULT_ROWS = 24;
const HELD_INPUT_CAP_BYTES = 64 * 1024;
const MONO_FONT_STACK = '"JetBrains Mono Variable", "SF Mono", Menlo, Consolas, "DejaVu Sans Mono", monospace';

export class WorkspaceTerminal {
  private terminal: GhosttyTerminalAdapter;
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
  private lastResize: TerminalSize;
  private requestedSize: TerminalSize | null = null;
  private echoedSize: TerminalSize | null = null;
  private pendingEchoedSize: TerminalSize | null = null;
  private replayPainted = false;
  private initialSnapshotParsed = false;
  private heldInput: HeldInput[] = [];
  private flushingInput = false;

  constructor(
    client: PmClient,
    host: HTMLElement,
    terminalId: bigint,
    themeController: TerminalThemeController,
    onIssue?: (reason: GhosttyRendererIssue | null) => void,
    onStatus?: (status: TerminalStreamStatus) => void,
    onCommand?: (command: string) => void,
  ) {
    const viewportKey = `t:${terminalId}`;
    this.viewport = loadTerminalViewport(viewportKey)
      ?? { bufferType: "normal", linesFromBottom: 0 };
    const rememberViewport = () => {
      this.viewport = captureTerminalViewport(this.terminal);
      saveTerminalViewport(viewportKey, this.viewport);
    };
    this.terminal = new GhosttyTerminalAdapter(
      host as HTMLDivElement,
      themeController.currentGhosttyTheme(),
      () => this.scheduleResize(),
      () => onIssue?.("unavailable"),
      () => {
        onIssue?.(null);
        this.scheduleResize();
      },
      (href) => activateTerminalLink(
        client.getState,
        { kind: "terminal", id: terminalId.toString() },
        href,
        () => {
          this.viewportRevision += 1;
          rememberViewport();
        },
      ),
      (href) => href.startsWith("pm:") || isSafeHttpUrl(href),
      { family: MONO_FONT_STACK, size: FONT_SIZE_PX },
    );
    const initialSize = measureFit(this.terminal) ?? { cols: DEFAULT_COLS, rows: DEFAULT_ROWS };
    this.lastResize = initialSize;
    this.requestedSize = initialSize;
    this.handle = client.openTerminal(terminalId, initialSize);
    this.sizePrompt = createTerminalSizePrompt(host, () => measureFit(this.terminal), (size) => {
      if (!documentVisible(document)) return;
      this.lastResize = size;
      this.requestedSize = size;
      this.handle.resize(size.cols, size.rows);
      this.flushHeldInput();
    });
    this.sizePrompt.reset();
    if (measureFit(this.terminal)) this.sizePrompt.requested(initialSize);
    const sizeEcho = this.handle.onViewerOwnership?.((ownership) => {
      this.sizePrompt.observe(ownership);
      this.flushHeldInput();
    }) ?? (() => {});
    const ptySize = this.handle.onPtySize((size) => {
      this.echoedSize = size;
      this.lastResize = size;
      const disposition = workspacePtyEchoDisposition(
        { cols: this.terminal.cols, rows: this.terminal.rows },
        this.pendingEchoedSize,
        size,
        this.active,
        this.replayPainted,
      );
      if (disposition === "current") {
        this.pendingEchoedSize = null;
      } else if (disposition === "snapshot") {
        this.pendingEchoedSize = size;
        this.handle.refreshSnapshot();
      } else if (disposition === "apply") {
        this.pendingEchoedSize = null;
        applyEchoedPtySize(this.terminal, size);
      }
      this.flushHeldInput();
    });
    const encoder = new TextEncoder();
    const data = this.terminal.onData((value) => {
      if (this.pendingWrites === 0) this.resizeBeforeInput();
      const input = {
        data: typeof value === "string" ? encoder.encode(value) : value,
        submitted: value === "\r",
      };
      this.flushHeldInput();
      const disposition = workspaceInputDisposition(this.inputReady(), this.heldInput.length);
      if (disposition === "send") {
        this.handle.input(input.data, input.submitted);
        return;
      }
      this.heldInput = holdInput(this.heldInput, input, HELD_INPUT_CAP_BYTES);
      this.flushHeldInput();
    });
    const command = onCommand ? registerForegroundCommandHandler(this.terminal, onCommand) : null;
    const clipboard = new TerminalClipboard(document);
    host.appendChild(clipboard.el as HTMLElement);
    clipboard.attach();
    const clipboardDispose = wireTerminalClipboard(this.terminal, clipboard);
    this.handle.connect((frame) => {
      const viewport = this.viewport;
      const revision = this.viewportRevision;
      const echoedSize = frame.replay ? this.pendingEchoedSize : null;
      if (echoedSize) this.pendingEchoedSize = null;
      this.pendingWrites += 1;
      writeTerminalFrame(
        this.terminal,
        frame,
        viewport,
        () => {
          this.pendingWrites -= 1;
          if (frame.replay) this.replayPainted = true;
          if (frame.replay && (frame.snapshot ?? true)) this.initialSnapshotParsed = true;
          if (!frame.replay && this.viewport.linesFromBottom > 0) rememberViewport();
          this.flushHeldInput();
        },
        () => this.viewportRevision === revision,
        () => {
          if (!echoedSize) return;
          applyEchoedPtySize(this.terminal, echoedSize);
          this.lastResize = echoedSize;
        },
      );
    });
    const scroll = this.terminal.onScroll(() => {
      if (this.pendingWrites > 0) return;
      rememberViewport();
    });
    const rememberUserViewport = () => {
      this.viewportRevision += 1;
      rememberViewport();
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
        const size = { cols, rows };
        this.sizePrompt.requested(size);
        this.requestedSize = size;
        this.lastResize = size;
        this.handle.resize(cols, rows);
        this.flushHeldInput();
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
      this.terminal.setVisible(visible && this.active);
    });
    this.observer = new ResizeObserver(() => this.scheduleResize());
    this.observer.observe(host);
    const unregisterTheme = themeController.register(this);
    this.disposers = [
      () => data.dispose(),
      () => scroll.dispose(),
      () => host.removeEventListener("wheel", rememberUserViewport),
      () => host.removeEventListener("pointerup", rememberUserViewport),
      () => host.removeEventListener("keyup", rememberKeyboardViewport),
      () => command?.dispose(),
      clipboardDispose,
      () => clipboard.dispose(),
      sizeEcho,
      ptySize,
      status,
      visibility,
      () => viewer.dispose(),
      unregisterTheme,
    ];
    this.scheduleResize();
  }

  setActive(active: boolean): void {
    this.active = active;
    this.terminal.setActive(active);
    this.viewer.setVisible(active && documentVisible(document));
  }

  focus(): void { this.terminal.focus(); }
  retry(): void { this.handle.retry(); }
  setTheme(theme: GhosttyTheme): void { this.terminal.setTheme(theme); }

  dispose(): void {
    this.heldInput = [];
    this.sizePrompt.dispose();
    this.observer.disconnect();
    cancelAnimationFrame(this.resizeFrame);
    for (const dispose of this.disposers) dispose();
    this.handle.close();
    this.terminal.dispose();
  }

  private resize(): void {
    this.sizePrompt.localChanged();
    if (this.sizePrompt.blocked()) return;
    fitFullWidth(this.terminal);
    const measured = measureFit(this.terminal);
    if (!measured) return;
    const requested = workspaceResizeRequest(measured, this.lastResize, this.handle.ptySize());
    if (!requested) return;
    this.sizePrompt.requested(requested);
    this.lastResize = requested;
    this.requestedSize = requested;
    this.handle.resize(requested.cols, requested.rows);
  }

  private inputReady(): boolean {
    const echoedSize = this.handle.ptySize() ?? this.echoedSize;
    return workspaceInputReady({
      initialSnapshotParsed: this.initialSnapshotParsed,
      pendingWrites: this.pendingWrites,
      pendingEchoedSize: this.pendingEchoedSize,
      requestedSize: this.requestedSize,
      echoedSize,
      promptBlocked: this.sizePrompt.blocked(),
      allowMissingEcho: echoedSize === null,
    });
  }

  private flushHeldInput(): void {
    if (this.flushingInput || this.heldInput.length === 0
      || this.pendingWrites > 0 || !this.initialSnapshotParsed) return;
    this.flushingInput = true;
    try {
      this.resizeBeforeInput();
      while (this.heldInput.length > 0 && this.inputReady()) {
        const held = this.heldInput;
        this.heldInput = [];
        for (const input of held) this.handle.input(input.data, input.submitted);
      }
    } finally {
      this.flushingInput = false;
    }
  }

  private resizeBeforeInput(): void {
    if (this.resizeFrame) {
      cancelAnimationFrame(this.resizeFrame);
      this.resizeFrame = 0;
    }
    this.resize();
  }

  private scheduleResize(): void {
    if (this.resizeFrame) return;
    this.resizeFrame = requestAnimationFrame(() => {
      this.resizeFrame = 0;
      this.resize();
    });
  }
}
