/**
 * Parsing the daemon's unified diff into the rows the review view
 * renders, and the small pure decisions around reading a review:
 * which file to jump to next, and how a thread anchors to a row.
 *
 * Kept apart from the component so the parts worth testing can be
 * tested without a DOM.
 */

import { markdownChoices } from "../components/markdownChoice";

export type RowKind = "context" | "add" | "del" | "hunk";

export interface DiffRow {
  kind: RowKind;
  /** Line number on the base side; null on an added line. */
  oldLine: number | null;
  /** Line number on the working side; null on a deleted line. */
  newLine: number | null;
  text: string;
}

export interface DiffFile {
  path: string;
  rows: DiffRow[];
  added: number;
  removed: number;
  /** True when the daemon reported the file as binary. */
  binary: boolean;
  /** True when the file was deleted. */
  deleted?: boolean;
  /** True when the file was newly added. */
  newFile?: boolean;
  /**
   * Which side the daemon could not read, when it could not read one.
   * The file has no rows in that case, and saying nothing would leave
   * the reader looking at a file that appears not to differ.
   */
  unreadable: string | null;
}

const FILE_HEADER = /^diff --git a\/(.+) b\/(.+)$/;
const HUNK_HEADER = /^@@ -(\d+)(?:,(\d+))? \+(\d+)(?:,(\d+))? @@/;

/**
 * Splits a unified diff into files and rows. Line numbers are tracked
 * per side so a comment can anchor to the working-tree line even when
 * it sits in a hunk full of deletions.
 */
export function parseDiff(text: string): DiffFile[] {
  const files: DiffFile[] = [];
  let file: DiffFile | null = null;
  let oldLine = 0;
  let newLine = 0;

  for (const line of text.split("\n")) {
    const header = FILE_HEADER.exec(line);
    if (header) {
      file = {
        path: header[2],
        rows: [],
        added: 0,
        removed: 0,
        binary: false,
        deleted: false,
        newFile: false,
        unreadable: null,
      };
      files.push(file);
      continue;
    }
    if (!file) continue;
    if (line.startsWith("deleted file")) {
      file.deleted = true;
      continue;
    }
    if (line.startsWith("new file")) {
      file.newFile = true;
      continue;
    }
    if (line.startsWith("Binary files")) {
      file.binary = true;
      continue;
    }
    if (line.startsWith("Unreadable: ")) {
      file.unreadable = line.slice("Unreadable: ".length);
      continue;
    }
    if (line.startsWith("--- ") || line.startsWith("+++ ")) continue;

    const hunk = HUNK_HEADER.exec(line);
    if (hunk) {
      oldLine = Number(hunk[1]);
      newLine = Number(hunk[3]);
      file.rows.push({ kind: "hunk", oldLine: null, newLine: null, text: line });
      continue;
    }
    if (line === "" && file.rows.length === 0) continue;

    const marker = line[0];
    const body = line.slice(1);
    if (marker === "+") {
      file.rows.push({ kind: "add", oldLine: null, newLine, text: body });
      newLine += 1;
      file.added += 1;
    } else if (marker === "-") {
      file.rows.push({ kind: "del", oldLine, newLine: null, text: body });
      oldLine += 1;
      file.removed += 1;
    } else if (marker === " ") {
      file.rows.push({ kind: "context", oldLine, newLine, text: body });
      oldLine += 1;
      newLine += 1;
    }
  }
  return files;
}

/**
 * The next file still needing attention below the one just cleared.
 *
 * Only below: wrapping around to a file above throws the reader back
 * up a review they are working down, which reads as the page losing
 * their place rather than as help.
 */
export function nextUnviewedFile(
  files: string[],
  current: string,
  viewed: ReadonlySet<string>,
): string | null {
  const start = files.indexOf(current);
  if (start < 0) return null;
  for (let i = start + 1; i < files.length; i += 1) {
    if (!viewed.has(files[i])) return files[i];
  }
  return null;
}

/** Scroll positions are per view, layout, and context: the same file at a
 * different context width is a different page. */
export function scrollKey(view: string, layout: string, context: number): string {
  return `${view}:${layout}:${context}`;
}

export type AnchorStatus = "same" | "moved" | "changed" | "unknown";

/**
 * What to tell the reader about a thread whose line may have shifted.
 * Silence when it has not moved: a note on every thread is noise.
 */
export function anchorNote(
  status: AnchorStatus,
  originalLine: number,
  currentLine: number,
): string | null {
  switch (status) {
    case "moved":
      return `moved from line ${originalLine} to ${currentLine}`;
    case "changed":
      return `this line changed since the comment was written (was line ${originalLine})`;
    case "unknown":
      return "this line could not be located in the current file";
    default:
      return null;
  }
}

export const MARKDOWN_EXT = /\.(md|markdown|mdown|mkd)$/i;

export function isMarkdown(path: string): boolean {
  return MARKDOWN_EXT.test(path);
}

export const IMAGE_EXT = /\.(png|jpe?g|gif|webp|ico|bmp|avif|svg)$/i;

export function isImage(path: string): boolean {
  return IMAGE_EXT.test(path);
}

/**
 * What the reader has said about previews, and what the review itself
 * says about the files in it.
 */
export interface PreviewDefaults {
  /** Files the reader turned the preview off on, from their viewer state. */
  previewOff: ReadonlySet<string>;
  /** Markdown files the reader turned the preview on for, browser-local. */
  markdownPreviewOn: ReadonlySet<string>;
  /** Markdown files whose rendered form carries a choice list. */
  choiceFiles: ReadonlySet<string>;
  /** The review is one document read on its own rather than a code range. */
  documentReview: boolean;
}

/**
 * Markdown in a code review reads as hunks, because that is what a
 * diff is for. A document that asks the reader to choose is the
 * exception: the choice card only exists inside the rendered form, so
 * leaving the preview off makes the question unanswerable.
 */
export function fileDefaultPreview(path: string, defaults: PreviewDefaults): boolean {
  if (defaults.previewOff.has(path)) return false;
  if (isImage(path)) return true;
  if (!isMarkdown(path)) return false;
  return (
    defaults.markdownPreviewOn.has(path) ||
    defaults.choiceFiles.has(path) ||
    defaults.documentReview
  );
}

/** The markdown files among those read whose rendered form asks a question. */
export function choiceBearingFiles(texts: Readonly<Record<string, string>>): Set<string> {
  const found = new Set<string>();
  for (const [path, text] of Object.entries(texts)) {
    if (isMarkdown(path) && markdownChoices(text).length > 0) found.add(path);
  }
  return found;
}

/**
 * The view a "changes for this thread" link opens: the round that
 * produced the reply, which is exactly the agent's response to the
 * last batch and nothing else.
 */
export function changesView(rev: number): string {
  return `round:${rev}`;
}

export interface ViewOption {
  value: string;
  label: string;
}

/**
 * The view picker's options, built from the revision pairs the daemon
 * recorded. A round only appears once both of its snapshots exist,
 * because a half-finished round has nothing to compare.
 */
export function viewOptions(
  revisions: { rev: number; kind: "sent" | "received" }[],
  current = "",
): ViewOption[] {
  const byRev = new Map<number, { sent?: boolean; received?: boolean }>();
  for (const r of revisions) {
    const entry = byRev.get(r.rev) ?? {};
    entry[r.kind] = true;
    byRev.set(r.rev, entry);
  }
  const options: ViewOption[] = [{ value: "", label: "working tree (live)" }];
  for (const rev of [...byRev.keys()].sort((a, b) => a - b)) {
    const entry = byRev.get(rev)!;
    if (entry.sent && entry.received) {
      options.push({ value: `round:${rev}`, label: `Rev ${rev} — agent changes` });
    }
    if (entry.received) {
      options.push({
        value: `delta:${rev}`,
        label: rev > 1 ? `Rev ${rev - 1}..Rev ${rev} only` : `base..Rev ${rev} only`,
      });
      options.push({ value: `cum:${rev}`, label: `base..Rev ${rev}` });
    }
    if (entry.sent) {
      options.push({ value: `sent:${rev}`, label: `base..Rev ${rev} (sent)` });
    }
  }
  // A stored view can outlive the revision behind it — a round that
  // produced no changes is no longer offered, for one. A select whose
  // value matches no option renders blank, which reads as the picker
  // having disappeared, so keep the current view listed and say it is
  // no longer available.
  if (current && !options.some((option) => option.value === current)) {
    options.splice(1, 0, { value: current, label: `${current} (no longer available)` });
  }
  return options;
}

/** Names the snapshot a rendered view came from. A comment written
 * against that render sends it back, so its line is read against the
 * revision on screen rather than the tree the comment lands on. */
export const SNAPSHOT_HEADER = "x-review-snapshot";

/** The snapshot a diff or file response was rendered from, or null when
 * the daemon named none. */
export function renderSnapshot(res: { headers: { get(name: string): string | null } }): bigint | null {
  const raw = res.headers.get(SNAPSHOT_HEADER);
  return raw !== null && /^\d+$/.test(raw) ? BigInt(raw) : null;
}

/** Names the newest tree the daemon had observed when it rendered. The
 * page asks what has changed since that one, so a reader on a stored
 * revision is measured against the working tree rather than against the
 * revision they chose to read. */
export const LIVE_SNAPSHOT_HEADER = "x-review-live";

/** The newest tree behind a render, or null when the daemon named none
 * and there is therefore nothing to compare against. */
export function liveSnapshot(res: { headers: { get(name: string): string | null } }): bigint | null {
  const raw = res.headers.get(LIVE_SNAPSHOT_HEADER);
  return raw !== null && /^\d+$/.test(raw) ? BigInt(raw) : null;
}

/** The files a reader has unsent text against.
 *
 * A comment draft is keyed by the line it sits on, and a reply draft by
 * the thread it answers, so the thread's own file is what makes a reply
 * count. Empty entries are cleared drafts and nobody is writing them. */
export function draftPaths(
  drafts: Readonly<Record<string, string>>,
  threadPaths: ReadonlyMap<number, string>,
): string[] {
  const paths = new Set<string>();
  for (const [key, body] of Object.entries(drafts)) {
    if (body.trim() === "") continue;
    if (key.startsWith("line:")) {
      // line:<path>:<line>:<side>, and a path may itself hold colons.
      const parts = key.slice("line:".length).split(":");
      if (parts.length > 2) paths.add(parts.slice(0, -2).join(":"));
    } else if (key.startsWith("reply:")) {
      const path = threadPaths.get(Number(key.slice("reply:".length)));
      if (path !== undefined) paths.add(path);
    }
  }
  return [...paths];
}

/** What to do about a tree that has moved under the reader.
 *
 * Replacing the diff is only safe for a reader who is reading the
 * working tree and has written nothing on a file that moved: anyone
 * else chose the revision they are looking at, or is mid-sentence
 * against a line that is about to shift, and is offered the newer tree
 * rather than moved onto it. */
export function staleAction(
  view: string,
  changed: readonly string[],
  writingOn: readonly string[],
): "none" | "refresh" | "offer" {
  if (changed.length === 0) return "none";
  if (view !== "") return "offer";
  const touched = new Set(changed);
  return writingOn.some((path) => touched.has(path)) ? "offer" : "refresh";
}

/** The source lines a rendered markdown block came from, which is what
 * a comment placed on that block was written against. */
export function previewExcerpt(text: string, range: { start: number; end: number }): string {
  return text
    .split("\n")
    .slice(Math.max(range.start - 1, 0), range.end)
    .join("\n");
}

/**
 * Adds a placeholder for every file a thread points at that the diff does
 * not carry, so a comment on a file an agent has since deleted still has
 * somewhere to render.
 *
 * `loaded` says whether a diff response has actually arrived. Before one
 * has, every path is absent, and treating that as deletion marks the whole
 * review deleted for one render — long enough for a file whose threads are
 * all resolved to mount folded and stay that way.
 */
export function withMissingThreadFiles(
  diff: DiffFile[],
  paths: readonly string[],
  loaded: boolean,
): DiffFile[] {
  if (!loaded) return diff;
  const seen = new Set(diff.map((file) => file.path));
  const extra: DiffFile[] = [];
  for (const path of paths) {
    if (seen.has(path)) continue;
    seen.add(path);
    extra.push({
      path,
      rows: [],
      added: 0,
      removed: 0,
      binary: false,
      deleted: true,
      newFile: false,
      unreadable: null,
    });
  }
  return extra.length > 0 ? [...diff, ...extra] : diff;
}

/** The daemon's own reason for refusing a review, falling back to the
 * status when it did not send one. */
export async function reviewReadError(res: Response, id: number | string): Promise<string> {
  try {
    const body = (await res.json()) as { error?: unknown };
    if (typeof body.error === "string" && body.error.trim() !== "") return body.error;
  } catch {
    // A non-JSON body says nothing useful; the status still does.
  }
  return `review ${id} could not be read (${res.status})`;
}
