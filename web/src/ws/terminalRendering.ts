import type { Terminal } from "@xterm/xterm";

interface RenderInternals {
  _core?: {
    _renderService?: {
      _handleIntersectionChange?: (entry: Pick<IntersectionObserverEntry, "isIntersecting" | "intersectionRatio">) => void;
    };
  };
}

/**
 * Resumes drawing a terminal whose screen has just come back into view.
 * xterm pauses a terminal its IntersectionObserver sees off screen and
 * resumes it only when that observer next reports, a frame after it is
 * shown. Resuming here lets the frame that shows it also draw it.
 */
export function resumeRendering(term: Terminal): void {
  (term as unknown as RenderInternals)._core?._renderService?._handleIntersectionChange?.({
    isIntersecting: true,
    intersectionRatio: 1,
  });
}
