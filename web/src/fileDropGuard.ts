import { isFileDrag } from "./views/sessionDrop";

interface DragLike {
  readonly dataTransfer: { readonly types: readonly string[] | DOMStringList } | null;
  readonly defaultPrevented: boolean;
  preventDefault(): void;
}

interface GuardedDocument {
  addEventListener(type: string, listener: (event: DragLike) => void): void;
  removeEventListener(type: string, listener: (event: DragLike) => void): void;
}

/**
 * Keeps a file dropped outside a terminal from navigating the tab to it,
 * which closes the dashboard and every terminal in it.
 *
 * A drop a terminal took has already had its default prevented, so this
 * only ever claims the ones nothing wanted.
 */
export function guardStrayFileDrops(doc: GuardedDocument): () => void {
  const guard = (event: DragLike) => {
    if (event.defaultPrevented) return;
    if (!event.dataTransfer || !isFileDrag(event.dataTransfer.types)) return;
    event.preventDefault();
  };
  doc.addEventListener("dragover", guard);
  doc.addEventListener("drop", guard);
  return () => {
    doc.removeEventListener("dragover", guard);
    doc.removeEventListener("drop", guard);
  };
}
