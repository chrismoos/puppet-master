// A choice list is an ordinary markdown task list preceded by an HTML
// comment naming it. The comment is invisible to any markdown renderer,
// so the raw document still reads as a plan, while the marker gives the
// two things a bare task list cannot: an unambiguous signal that this
// list asks a question rather than tracking todos, and a stable id the
// agent can match on after a label is reworded.
//
// Without the marker nothing here applies, which is the point: a real
// todo list in a plan must never become clickable.

export type ChoiceSelect = "one" | "many";

export interface ChoiceOption {
  /** Slug of the label, stable while the label is. */
  id: string;
  label: string;
  /** Indented lines under the option, joined into one paragraph. */
  detail: string;
  /** The box was already ticked in the source. */
  checked: boolean;
  /** 1-based source line of the option's own bullet. */
  line: number;
}

/** A marked task list and its 1-based, inclusive source range. The
 *  range starts at the marker comment, so the whole construct — the
 *  question and every option — is one thing to anchor against. */
export interface ChoiceList {
  id: string;
  select: ChoiceSelect;
  options: ChoiceOption[];
  start: number;
  end: number;
}

const MARKER = /^<!--\s*pm-choice\s+(.*?)\s*-->$/;
const ATTRIBUTE = /([a-zA-Z][\w-]*)=("[^"]*"|'[^']*'|\S+)/g;
const TASK_ITEM = /^[-*]\s+\[([ xX])\]\s*(.*)$/;
const INDENTED = /^[ \t]+\S/;

/** The free-text option, by the label the marker syntax gives it. */
export const OTHER_OPTION_ID = "other";

export function markdownChoices(text: string): ChoiceList[] {
  const lines = text.split(/\r?\n/);
  const found: ChoiceList[] = [];
  for (let i = 0; i < lines.length; i += 1) {
    const attrs = markerAttributes(lines[i]);
    if (!attrs) continue;
    const id = attrs.get("id");
    if (!id) continue;
    const parsed = readOptions(lines, i + 1);
    // A marker with no task list under it marks nothing, so it is left
    // to render as the stray comment it is.
    if (parsed.options.length === 0) continue;
    found.push({
      id,
      select: attrs.get("select") === "many" ? "many" : "one",
      options: parsed.options,
      start: i + 1,
      end: parsed.end,
    });
    i = parsed.end - 1;
  }
  return found;
}

function markerAttributes(line: string): Map<string, string> | null {
  const marker = line.trim().match(MARKER);
  if (!marker) return null;
  const attrs = new Map<string, string>();
  for (const [, key, raw] of marker[1].matchAll(ATTRIBUTE)) {
    attrs.set(key, raw.replace(/^["']|["']$/g, ""));
  }
  return attrs;
}

/**
 * Reads the task list under a marker. An indented line belongs to the
 * option above it as that option's detail, and a blank line only ends
 * the list if nothing that belongs to it follows.
 */
function readOptions(lines: string[], from: number): { options: ChoiceOption[]; end: number } {
  const options: ChoiceOption[] = [];
  const detail: string[][] = [];
  const taken = new Set<string>();
  let last = from;
  let i = from;
  while (i < lines.length) {
    const line = lines[i];
    const item = line.match(TASK_ITEM);
    if (item) {
      const label = item[2].trim();
      options.push({
        id: uniqueSlug(label, taken),
        label,
        detail: "",
        checked: item[1] !== " ",
        line: i + 1,
      });
      detail.push([]);
      last = i + 1;
    } else if (INDENTED.test(line) && options.length > 0) {
      detail[detail.length - 1].push(line.trim());
      last = i + 1;
    } else if (line.trim() !== "") {
      break;
    }
    i += 1;
  }
  options.forEach((option, n) => {
    option.detail = detail[n].join(" ");
  });
  return { options, end: Math.max(from, last) };
}

/** Two options that read the same still have to answer to different
 *  ids, or an answer cannot say which one it means. */
function uniqueSlug(label: string, taken: Set<string>): string {
  const base = slug(label);
  let id = base;
  for (let n = 2; taken.has(id); n += 1) id = `${base}-${n}`;
  taken.add(id);
  return id;
}

function slug(label: string): string {
  const out = label
    .toLowerCase()
    .replace(/[^a-z0-9]+/g, "-")
    .replace(/^-+|-+$/g, "");
  return out || "option";
}

export function isOtherOption(option: ChoiceOption): boolean {
  return option.id === OTHER_OPTION_ID;
}
