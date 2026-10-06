import { Fragment, type ReactNode } from "react";
import { classifyHref, type PmLink } from "@puppet-master/client-core/pmlink";
import { markdownChoices, type ChoiceList } from "./markdownChoice";

// Minimal markdown for agent-authored briefings and item bodies,
// rendered as React elements so no HTML is ever injected. Covers
// headings, nested lists, tables, paragraphs, code fences, inline
// bold/italic/code, and links; pm: links route through onPmLink,
// everything else opens in a new tab.

/** A block's 1-based, inclusive source line range. */
export interface BlockRange {
  start: number;
  end: number;
}

export interface MarkdownProps {
  text: string;
  onPmLink: (link: PmLink) => void;
  /** Wraps every anchorable region with the source lines it came from, so
   *  a rendered document can anchor comments the way a diff line does.
   *  A list is anchored per item rather than as a whole, because a bullet
   *  is what a reader points at; the wrapper then sits inside the <li>,
   *  which keeps the list a list. A marked choice list arrives whole,
   *  with what the marker said, so a caller can render it as a control
   *  instead of as text. */
  renderBlock?: (block: ReactNode, range: BlockRange, choice?: ChoiceList) => ReactNode;
}

export function Markdown({ text, onPmLink, renderBlock }: MarkdownProps) {
  return <div className="markdown">{renderBlocks(text, onPmLink, renderBlock)}</div>;
}

/** One top-level block: its kind, its lines, and where it came from. */
interface Block extends BlockRange {
  kind: "fence" | "heading" | "list" | "paragraph" | "choice" | "table";
  lines: string[];
  level?: number;
  ordered?: boolean;
  choice?: ChoiceList;
  list?: MarkdownList;
  table?: MarkdownTable;
}

interface MarkdownList {
  ordered: boolean;
  start?: number;
  items: MarkdownListItem[];
}

interface MarkdownListItem {
  text: string;
  line: number;
  children: MarkdownList[];
}

type TableAlignment = "left" | "center" | "right" | null;

interface MarkdownTable {
  header: string[];
  alignments: TableAlignment[];
  rows: { cells: string[]; line: number }[];
}

interface ListLine {
  indent: number;
  ordered: boolean;
  start?: number;
  text: string;
}

/**
 * Splits markdown into top-level blocks with the source lines each came
 * from. Exported so a caller can anchor to a block before rendering it,
 * and so the ranges are covered directly by tests.
 */
export function markdownBlocks(text: string): Block[] {
  const lines = text.split(/\r?\n/);
  const marked = new Map(markdownChoices(text).map((c) => [c.start, c]));
  const blocks: Block[] = [];
  let i = 0;
  while (i < lines.length) {
    const line = lines[i];
    if (line.trim() === "") {
      i += 1;
      continue;
    }
    const from = i;
    // The marker and the list it marks are one construct, so they are
    // one block: the answer belongs to the question, not to a bullet.
    const choice = marked.get(i + 1);
    if (choice) {
      blocks.push({
        kind: "choice",
        lines: lines.slice(i, choice.end),
        start: choice.start,
        end: choice.end,
        choice,
      });
      i = choice.end;
      continue;
    }
    const at = (kind: Block["kind"], body: string[], extra: Partial<Block> = {}) =>
      blocks.push({ kind, lines: body, start: from + 1, end: Math.max(from + 1, i), ...extra });
    if (line.startsWith("```")) {
      const code: string[] = [];
      i += 1;
      while (i < lines.length && !lines[i].startsWith("```")) {
        code.push(lines[i]);
        i += 1;
      }
      i += 1;
      at("fence", code);
      continue;
    }
    const heading = line.match(/^(#{1,4})\s+(.*)$/);
    if (heading) {
      i += 1;
      at("heading", [heading[2]], { level: heading[1].length });
      continue;
    }
    const table = readTable(lines, i);
    if (table) {
      i = table.next;
      at("table", lines.slice(from, i), { table: table.table });
      continue;
    }
    const firstListLine = parseListLine(line);
    if (firstListLine) {
      const parsed = readList(lines, i, firstListLine.indent, firstListLine.ordered);
      i = parsed.next;
      at("list", lines.slice(from, i), {
        ordered: parsed.list.ordered,
        list: parsed.list,
      });
      continue;
    }
    const paragraph: string[] = [];
    while (
      i < lines.length &&
      lines[i].trim() !== "" &&
      !lines[i].startsWith("```") &&
      !/^#{1,4}\s/.test(lines[i]) &&
      !parseListLine(lines[i]) &&
      !readTable(lines, i)
    ) {
      paragraph.push(lines[i]);
      i += 1;
    }
    at("paragraph", paragraph);
  }
  return blocks;
}

function parseListLine(line: string): ListLine | null {
  const match = line.match(/^([ \t]*)([-*+]|(\d+)[.)])[ \t]+(.*)$/);
  if (!match) return null;
  let indent = 0;
  for (const character of match[1]) indent += character === "\t" ? 4 : 1;
  const start = match[3] ? parseInt(match[3], 10) : undefined;
  return { indent, ordered: match[3] !== undefined, start, text: match[4] };
}

function lineIndent(line: string): number {
  let indent = 0;
  for (const character of line) {
    if (character === " ") indent += 1;
    else if (character === "\t") indent += 4;
    else break;
  }
  return indent;
}

function isBlockStart(lines: string[], index: number): boolean {
  const line = lines[index];
  if (!line) return false;
  if (/^#{1,4}\s/.test(line)) return true;
  if (line.startsWith("```")) return true;
  if (line.trim().startsWith("<!-- pm-choice")) return true;
  if (readTable(lines, index) !== null) return true;
  return false;
}

function readList(
  lines: string[],
  from: number,
  indent: number,
  ordered: boolean,
): { list: MarkdownList; next: number } {
  const items: MarkdownListItem[] = [];
  let i = from;
  let last = from;
  let start: number | undefined;

  while (i < lines.length) {
    if (lines[i].trim() === "") {
      let j = i;
      while (j < lines.length && lines[j].trim() === "") j += 1;
      if (j >= lines.length || isBlockStart(lines, j)) break;
      const lookahead = parseListLine(lines[j]);
      if (lookahead && lookahead.indent === indent && lookahead.ordered === ordered) {
        i = j;
      } else {
        break;
      }
    }

    const parsed = parseListLine(lines[i]);
    if (!parsed || parsed.indent !== indent || parsed.ordered !== ordered) break;
    if (start === undefined && parsed.start !== undefined) {
      start = parsed.start;
    }

    const item: MarkdownListItem = { text: parsed.text, line: i + 1, children: [] };
    items.push(item);
    i += 1;
    last = i;

    while (i < lines.length) {
      if (lines[i].trim() === "") {
        let j = i;
        while (j < lines.length && lines[j].trim() === "") j += 1;
        if (j >= lines.length || isBlockStart(lines, j)) break;
        const lookahead = parseListLine(lines[j]);
        if (lookahead && lookahead.indent === indent && lookahead.ordered === ordered) {
          break;
        }
        if (lookahead && lookahead.indent > indent) {
          i = j;
        } else if (!lookahead && lineIndent(lines[j]) > indent) {
          i = j;
        } else {
          break;
        }
      }

      if (isBlockStart(lines, i)) break;

      const child = parseListLine(lines[i]);
      if (child) {
        if (child.indent > indent) {
          const nested = readList(lines, i, child.indent, child.ordered);
          item.children.push(nested.list);
          i = nested.next;
          last = i;
          continue;
        }
        break;
      }

      if (lineIndent(lines[i]) > indent) {
        item.text += (item.text ? " " : "") + lines[i].trim();
        i += 1;
        last = i;
        continue;
      }

      break;
    }
  }

  const list: MarkdownList = { ordered, items };
  if (ordered && start !== undefined) {
    list.start = start;
  }
  return { list, next: last };
}

function readTable(
  lines: string[],
  from: number,
): { table: MarkdownTable; next: number } | null {
  if (from + 1 >= lines.length || !lines[from].includes("|")) return null;
  const header = splitTableRow(lines[from]);
  const separators = splitTableRow(lines[from + 1]);
  if (
    header.length === 0 ||
    separators.length !== header.length ||
    separators.some((cell) => !/^:?-{3,}:?$/.test(cell.trim()))
  ) {
    return null;
  }
  const alignments = separators.map<TableAlignment>((cell) => {
    const value = cell.trim();
    if (value.startsWith(":")) return value.endsWith(":") ? "center" : "left";
    return value.endsWith(":") ? "right" : null;
  });
  const rows: MarkdownTable["rows"] = [];
  let i = from + 2;
  while (i < lines.length && lines[i].trim() !== "" && lines[i].includes("|")) {
    const cells = splitTableRow(lines[i]);
    rows.push({
      cells: header.map((_, column) => cells[column] ?? ""),
      line: i + 1,
    });
    i += 1;
  }
  return { table: { header, alignments, rows }, next: i };
}

function splitTableRow(line: string): string[] {
  let value = line.trim();
  if (value.startsWith("|")) value = value.slice(1);
  if (value.endsWith("|") && !value.endsWith("\\|")) value = value.slice(0, -1);
  const cells: string[] = [];
  let cell = "";
  let inCode = false;
  for (let i = 0; i < value.length; i += 1) {
    const character = value[i];
    if (character === "\\" && value[i + 1] === "|") {
      cell += "|";
      i += 1;
    } else if (character === "`") {
      inCode = !inCode;
      cell += character;
    } else if (character === "|" && !inCode) {
      cells.push(cell.trim());
      cell = "";
    } else {
      cell += character;
    }
  }
  cells.push(cell.trim());
  return cells;
}

/**
 * Every anchorable range, in the order the document renders them. A list
 * contributes one per item; every other block contributes itself.
 *
 * The renderer and anything mapping existing threads back onto the page
 * must agree about what an anchor is. When they disagree a comment made
 * on one bullet reappears under another, so both read this.
 */
export function markdownAnchors(text: string): BlockRange[] {
  const anchors: BlockRange[] = [];
  for (const block of markdownBlocks(text)) {
    if (block.kind === "list") {
      appendListAnchors(block.list!, anchors);
    } else {
      anchors.push({ start: block.start, end: block.end });
    }
  }
  return anchors;
}

function appendListAnchors(list: MarkdownList, anchors: BlockRange[]) {
  for (const item of list.items) {
    anchors.push({ start: item.line, end: item.line });
    item.children.forEach((child) => appendListAnchors(child, anchors));
  }
}

function renderBlocks(
  text: string,
  onPmLink: (link: PmLink) => void,
  renderBlock?: (block: ReactNode, range: BlockRange, choice?: ChoiceList) => ReactNode,
): ReactNode[] {
  return markdownBlocks(text).map((b, key) => {
    let node: ReactNode;
    if (b.kind === "fence") {
      node = (
        <pre key={key}>
          <code>{b.lines.join("\n")}</code>
        </pre>
      );
    } else if (b.kind === "heading") {
      const Tag = (["h1", "h2", "h3", "h4"] as const)[(b.level ?? 1) - 1];
      node = <Tag key={key}>{renderInline(b.lines[0], onPmLink)}</Tag>;
    } else if (b.kind === "choice") {
      // Without a caller that renders it as a control this stays what
      // the raw document is, an ordinary task list, marker and all.
      node = (
        <ul key={key}>
          {b.choice!.options.map((o) => (
            <li key={o.id}>
              {`[${o.checked ? "x" : " "}] `}
              {renderInline(o.label, onPmLink)}
              {o.detail !== "" && <div>{renderInline(o.detail, onPmLink)}</div>}
            </li>
          ))}
        </ul>
      );
    } else if (b.kind === "list") {
      const list = renderList(b.list!, onPmLink, renderBlock);
      // Already anchored per item, so it is not wrapped again as a whole.
      return <Fragment key={key}>{list}</Fragment>;
    } else if (b.kind === "table") {
      node = (
        <div className="markdown-table-wrap" key={key}>
          <table>
            <thead>
              <tr>
                {b.table!.header.map((cell, column) => (
                  <th key={column} style={{ textAlign: b.table!.alignments[column] ?? undefined }}>
                    {renderInline(cell, onPmLink)}
                  </th>
                ))}
              </tr>
            </thead>
            <tbody>
              {b.table!.rows.map((row) => (
                <tr key={row.line}>
                  {row.cells.map((cell, column) => (
                    <td key={column} style={{ textAlign: b.table!.alignments[column] ?? undefined }}>
                      {renderInline(cell, onPmLink)}
                    </td>
                  ))}
                </tr>
              ))}
            </tbody>
          </table>
        </div>
      );
    } else {
      node = <p key={key}>{renderInline(b.lines.join(" "), onPmLink)}</p>;
    }
    if (!renderBlock) return node;
    return (
      <Fragment key={key}>
        {renderBlock(node, { start: b.start, end: b.end }, b.choice)}
      </Fragment>
    );
  });
}

function renderList(
  list: MarkdownList,
  onPmLink: (link: PmLink) => void,
  renderBlock?: (block: ReactNode, range: BlockRange, choice?: ChoiceList) => ReactNode,
): ReactNode {
  const items = list.items.map((item) => {
    const content = renderInline(item.text, onPmLink);
    return (
      <li key={item.line}>
        {renderBlock ? renderBlock(content, { start: item.line, end: item.line }) : content}
        {item.children.map((child, index) => (
          <Fragment key={index}>{renderList(child, onPmLink, renderBlock)}</Fragment>
        ))}
      </li>
    );
  });
  const start =
    list.ordered && list.start !== undefined && list.start !== 1 ? list.start : undefined;
  return list.ordered ? <ol start={start}>{items}</ol> : <ul>{items}</ul>;
}

const INLINE_TOKEN =
  /\[([^\]]+)\]\(([^)\s]+)\)|`([^`]+)`|\*\*([^*]+)\*\*|\*([^*]+)\*/g;

function renderInline(text: string, onPmLink: (link: PmLink) => void): ReactNode[] {
  const nodes: ReactNode[] = [];
  let last = 0;
  let key = 0;
  for (const match of text.matchAll(INLINE_TOKEN)) {
    const index = match.index;
    if (index > last) nodes.push(<Fragment key={key++}>{text.slice(last, index)}</Fragment>);
    const [, linkText, href, code, bold, italic] = match;
    if (linkText !== undefined && href !== undefined) {
      nodes.push(
        <MdLink key={key++} text={linkText} href={href} onPmLink={onPmLink} />,
      );
    } else if (code !== undefined) {
      nodes.push(<code key={key++}>{code}</code>);
    } else if (bold !== undefined) {
      nodes.push(<strong key={key++}>{bold}</strong>);
    } else if (italic !== undefined) {
      nodes.push(<em key={key++}>{italic}</em>);
    }
    last = index + match[0].length;
  }
  if (last < text.length) nodes.push(<Fragment key={key++}>{text.slice(last)}</Fragment>);
  return nodes;
}

function MdLink({
  text,
  href,
  onPmLink,
}: {
  text: string;
  href: string;
  onPmLink: (link: PmLink) => void;
}) {
  const classified = classifyHref(href);
  switch (classified.kind) {
    case "pm":
      return (
        <a
          href="#pm-link"
          onClick={(e) => {
            e.preventDefault();
            onPmLink(classified.link);
          }}
        >
          {text}
        </a>
      );
    case "external":
      return (
        <a href={href} target="_blank" rel="noopener noreferrer">
          {text}
        </a>
      );
    case "inert":
      return <span>{text}</span>;
  }
}
