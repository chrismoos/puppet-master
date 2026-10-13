import { SESSION_DROP_FILE_MAX } from "../api/drops";

/** A drag carrying files rather than a session, a pane or a tab. */
export function isFileDrag(types: readonly string[] | DOMStringList): boolean {
  for (const type of Array.from(types as readonly string[])) {
    if (type === "Files") return true;
  }
  return false;
}

export interface DropItem {
  kind: string;
  webkitGetAsEntry?: () => { isDirectory?: boolean } | null;
}

export interface DropSelection {
  files: File[];
  /** Directories are refused: a drop carries their entries, not an
   * archive, and expanding one silently uploads a tree. */
  directories: number;
  oversized: string[];
}

/** What a drop offers, with directories and oversized files set aside so
 * the reason one was skipped can be shown rather than guessed at. */
export function dropSelection(
  files: readonly File[],
  items?: readonly DropItem[],
): DropSelection {
  const selection: DropSelection = { files: [], directories: 0, oversized: [] };
  files.forEach((file, index) => {
    const entry = items?.[index]?.webkitGetAsEntry?.();
    if (entry?.isDirectory) {
      selection.directories += 1;
      return;
    }
    // A directory dropped where the entry API is unavailable arrives as a
    // zero-byte file with no type, which is also what an empty file is;
    // either way there is nothing to store.
    if (file.size === 0) {
      selection.directories += 1;
      return;
    }
    if (file.size > SESSION_DROP_FILE_MAX) {
      selection.oversized.push(file.name);
      return;
    }
    selection.files.push(file);
  });
  return selection;
}

export type DropState =
  | { kind: "idle" }
  | { kind: "over" }
  | { kind: "uploading"; name: string; done: number; total: number }
  | { kind: "failed"; reason: string };

export function dropStatusText(state: DropState): string {
  switch (state.kind) {
    case "idle":
      return "";
    case "over":
      return "Drop here to insert file path for agent";
    case "uploading":
      return state.total > 1
        ? `Storing ${state.name} (${state.done + 1} of ${state.total})…`
        : `Storing ${state.name}…`;
    case "failed":
      return state.reason;
  }
}

/** The one-line reason for what a drop refused to take, or empty when it
 * took everything. */
export function skippedReason(selection: DropSelection): string {
  const parts: string[] = [];
  if (selection.directories > 0) {
    parts.push(
      selection.directories === 1
        ? "a folder was skipped"
        : `${selection.directories} folders were skipped`,
    );
  }
  if (selection.oversized.length > 0) {
    const limit = SESSION_DROP_FILE_MAX / (1024 * 1024);
    parts.push(`${selection.oversized.join(", ")} is over the ${limit} MB limit`);
  }
  return parts.join(", ");
}
