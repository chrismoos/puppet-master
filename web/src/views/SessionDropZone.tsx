import { type DragEvent, useCallback, useEffect, useRef, useState } from "react";
import { uploadSessionDrop } from "../api/drops";
import {
  type DropState,
  dropSelection,
  dropStatusText,
  isFileDrag,
  skippedReason,
} from "./sessionDrop";

/** How long a refusal stays on screen. Long enough to read a filename. */
const FAILED_NOTICE_MS = 8_000;

export interface SessionDropHandlers {
  onDragOver: (event: DragEvent<HTMLElement>) => void;
  onDragLeave: (event: DragEvent<HTMLElement>) => void;
  onDrop: (event: DragEvent<HTMLElement>) => void;
}

export interface SessionDrop {
  state: DropState;
  handlers: SessionDropHandlers;
  /** Whether this drag is one for files, so a caller with drags of its
   * own can leave the rest alone. */
  claims: (event: DragEvent<HTMLElement>) => boolean;
}

/**
 * Accepts files dropped on a session's terminal. The daemon stores each
 * one on the host running the session and types its path into the
 * composer, so nothing here touches the terminal itself.
 */
export function useSessionDrop(
  sessionId: string | null,
  terminalId: string | null,
): SessionDrop {
  const [state, setState] = useState<DropState>({ kind: "idle" });
  const timer = useRef<ReturnType<typeof setTimeout> | null>(null);
  useEffect(
    () => () => {
      if (timer.current) clearTimeout(timer.current);
    },
    [],
  );

  const fail = useCallback((reason: string) => {
    setState({ kind: "failed", reason });
    if (timer.current) clearTimeout(timer.current);
    timer.current = setTimeout(() => setState({ kind: "idle" }), FAILED_NOTICE_MS);
  }, []);

  const claims = useCallback(
    (event: DragEvent<HTMLElement>) => Boolean(sessionId) && isFileDrag(event.dataTransfer.types),
    [sessionId],
  );

  const onDragOver = useCallback(
    (event: DragEvent<HTMLElement>) => {
      if (!claims(event)) return;
      event.preventDefault();
      event.dataTransfer.dropEffect = "copy";
      setState((current) => (current.kind === "uploading" ? current : { kind: "over" }));
    },
    [claims],
  );

  const onDragLeave = useCallback((event: DragEvent<HTMLElement>) => {
    if (event.currentTarget.contains(event.relatedTarget as Node | null)) return;
    setState((current) => (current.kind === "over" ? { kind: "idle" } : current));
  }, []);

  const onDrop = useCallback(
    (event: DragEvent<HTMLElement>) => {
      if (!claims(event) || !sessionId) return;
      event.preventDefault();
      const selection = dropSelection(
        Array.from(event.dataTransfer.files),
        Array.from(event.dataTransfer.items ?? []),
      );
      const skipped = skippedReason(selection);
      if (selection.files.length === 0) {
        fail(skipped || "nothing in that drop could be stored");
        return;
      }
      void (async () => {
        for (const [index, file] of selection.files.entries()) {
          setState({
            kind: "uploading",
            name: file.name,
            done: index,
            total: selection.files.length,
          });
          try {
            // Sequential on purpose: each path is typed where the cursor
            // is, so two at once could interleave in the composer.
            const result = await uploadSessionDrop(sessionId, terminalId, file);
            if (!result.typed) {
              fail(`stored at ${result.path}, but ${result.detail}`);
              return;
            }
          } catch (error) {
            fail(error instanceof Error ? error.message : String(error));
            return;
          }
        }
        if (skipped) fail(skipped);
        else setState({ kind: "idle" });
      })();
    },
    [claims, fail, sessionId, terminalId],
  );

  return { state, handlers: { onDragOver, onDragLeave, onDrop }, claims };
}

export function SessionDropOverlay({ state }: { state: DropState }): React.ReactElement | null {
  if (state.kind === "idle") return null;
  const text = dropStatusText(state);
  return (
    <div className={`term-drop-overlay is-${state.kind}`} role="status">
      <div className="term-drop-card">{text}</div>
    </div>
  );
}
