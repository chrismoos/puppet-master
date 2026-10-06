/** The subset of Document the page visibility watcher reads. */
export interface VisibilityDocument {
  readonly visibilityState: string;
  addEventListener(type: "visibilitychange", listener: () => void): void;
  removeEventListener(type: "visibilitychange", listener: () => void): void;
}

export function documentVisible(doc: VisibilityDocument): boolean {
  return doc.visibilityState !== "hidden";
}

/**
 * Reports page visibility edges, collapsing repeated events in the same
 * state so listeners only hear real hidden/visible transitions. Returns
 * the unsubscribe function.
 */
export function watchDocumentVisibility(
  doc: VisibilityDocument,
  onChange: (visible: boolean) => void,
): () => void {
  let visible = documentVisible(doc);
  const listener = () => {
    const next = documentVisible(doc);
    if (next === visible) return;
    visible = next;
    onChange(next);
  };
  doc.addEventListener("visibilitychange", listener);
  return () => doc.removeEventListener("visibilitychange", listener);
}
