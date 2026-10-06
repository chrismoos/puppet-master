import hljs from "highlight.js/lib/core";
import bash from "highlight.js/lib/languages/bash";
import c from "highlight.js/lib/languages/c";
import cpp from "highlight.js/lib/languages/cpp";
import csharp from "highlight.js/lib/languages/csharp";
import css from "highlight.js/lib/languages/css";
import diff from "highlight.js/lib/languages/diff";
import dockerfile from "highlight.js/lib/languages/dockerfile";
import go from "highlight.js/lib/languages/go";
import ini from "highlight.js/lib/languages/ini";
import java from "highlight.js/lib/languages/java";
import javascript from "highlight.js/lib/languages/javascript";
import json from "highlight.js/lib/languages/json";
import kotlin from "highlight.js/lib/languages/kotlin";
import lua from "highlight.js/lib/languages/lua";
import makefile from "highlight.js/lib/languages/makefile";
import markdown from "highlight.js/lib/languages/markdown";
import objectivec from "highlight.js/lib/languages/objectivec";
import perl from "highlight.js/lib/languages/perl";
import php from "highlight.js/lib/languages/php";
import protobuf from "highlight.js/lib/languages/protobuf";
import python from "highlight.js/lib/languages/python";
import ruby from "highlight.js/lib/languages/ruby";
import rust from "highlight.js/lib/languages/rust";
import scss from "highlight.js/lib/languages/scss";
import shell from "highlight.js/lib/languages/shell";
import sql from "highlight.js/lib/languages/sql";
import swift from "highlight.js/lib/languages/swift";
import typescript from "highlight.js/lib/languages/typescript";
import xml from "highlight.js/lib/languages/xml";
import yaml from "highlight.js/lib/languages/yaml";

// Registered explicitly rather than importing the full build, which
// carries every grammar highlight.js ships and most of a megabyte.
const LANGUAGES: Record<string, LanguageFn> = {
  bash, c, cpp, csharp, css, diff, dockerfile, go, ini, java, javascript,
  json, kotlin, lua, makefile, markdown, objectivec, perl, php, protobuf,
  python, ruby, rust, scss, shell, sql, swift, typescript, xml, yaml,
};
type LanguageFn = Parameters<typeof hljs.registerLanguage>[1];

let registered = false;
function ensureRegistered(): void {
  if (registered) return;
  for (const [name, fn] of Object.entries(LANGUAGES)) hljs.registerLanguage(name, fn);
  registered = true;
}

/// Extensions that do not simply name their grammar, and filenames that
/// carry no extension at all.
const BY_EXTENSION: Record<string, string> = {
  ts: "typescript", tsx: "typescript", mts: "typescript", cts: "typescript",
  js: "javascript", jsx: "javascript", mjs: "javascript", cjs: "javascript",
  rs: "rust", py: "python", rb: "ruby", kt: "kotlin", kts: "kotlin",
  h: "c", hpp: "cpp", cc: "cpp", cxx: "cpp", m: "objectivec", mm: "objectivec",
  cs: "csharp", pl: "perl", sh: "bash", zsh: "bash", bash: "bash",
  html: "xml", htm: "xml", svg: "xml", vue: "xml",
  yml: "yaml", toml: "ini", cfg: "ini", conf: "ini",
  md: "markdown", markdown: "markdown", proto: "protobuf",
  patch: "diff", mk: "makefile",
};

const BY_FILENAME: Record<string, string> = {
  dockerfile: "dockerfile",
  makefile: "makefile",
  "cargo.lock": "ini",
  gemfile: "ruby",
  rakefile: "ruby",
};

/// The grammar to highlight a path with, or null to leave it plain.
/// Guessing wrong is worse than not highlighting: a mis-detected grammar
/// colours the wrong words confidently, so only known mappings count and
/// automatic detection is deliberately not used.
export function languageForPath(path: string): string | null {
  const name = path.split("/").pop()?.toLowerCase() ?? "";
  const byName = BY_FILENAME[name];
  if (byName) return byName;
  const dot = name.lastIndexOf(".");
  if (dot <= 0) return null;
  const extension = name.slice(dot + 1);
  const mapped = BY_EXTENSION[extension] ?? extension;
  return mapped in LANGUAGES ? mapped : null;
}

/// Splits highlight.js output into one HTML string per line, carrying any
/// span open at the newline across it.
///
/// A token can span lines — a block comment, a template literal, a raw
/// string — and each diff row is its own element, so a span left open at
/// the end of a row would bleed into everything after it. Each line is
/// closed and the same tags reopened on the next.
export function splitHighlightedLines(html: string): string[] {
  const lines: string[] = [];
  const open: string[] = [];
  let current = "";
  const token = /(<span\b[^>]*>)|(<\/span>)|([^<]+)/g;
  let match: RegExpExecArray | null;
  while ((match = token.exec(html)) !== null) {
    const [, openTag, closeTag, text] = match;
    if (openTag) {
      open.push(openTag);
      current += openTag;
    } else if (closeTag) {
      open.pop();
      current += closeTag;
    } else if (text !== undefined) {
      const parts = text.split("\n");
      for (let i = 0; i < parts.length; i += 1) {
        if (i > 0) {
          current += "</span>".repeat(open.length);
          lines.push(current);
          current = open.join("");
        }
        current += parts[i];
      }
    }
  }
  lines.push(current);
  return lines;
}

/// Highlights contiguous source and returns one HTML string per line.
///
/// The whole text is highlighted in one pass rather than line by line,
/// because a line lifted out of its surroundings cannot tell that it sits
/// inside a block comment or a template literal and would be coloured as
/// though it were code.
export function highlightSide(code: string, language: string | null): string[] | null {
  if (!language) return null;
  ensureRegistered();
  if (!hljs.getLanguage(language)) return null;
  try {
    const { value } = hljs.highlight(code, { language, ignoreIllegals: true });
    return splitHighlightedLines(value);
  } catch {
    // A grammar that throws should cost the colours, never the diff.
    return null;
  }
}

/// Highlighted HTML for each row of one file, or null where a row should
/// stay plain.
///
/// Each side is reassembled and highlighted as contiguous source, then
/// indexed back onto the rows it came from. A hunk header is not code and
/// a row whose side failed to highlight keeps its plain text, so a
/// grammar that copes with one side of a file still colours the other.
export function highlightFileRows(
  path: string,
  rows: { kind: string; text: string }[],
): (string | null)[] {
  const language = languageForPath(path);
  const out: (string | null)[] = rows.map(() => null);
  if (!language) return out;

  const oldRows: number[] = [];
  const newRows: number[] = [];
  const oldSource: string[] = [];
  const newSource: string[] = [];
  rows.forEach((row, index) => {
    if (row.kind === "context" || row.kind === "del") {
      oldRows.push(index);
      oldSource.push(row.text);
    }
    if (row.kind === "context" || row.kind === "add") {
      newRows.push(index);
      newSource.push(row.text);
    }
  });

  const oldLines = highlightSide(oldSource.join("\n"), language);
  if (oldLines) oldRows.forEach((row, i) => (out[row] = oldLines[i] ?? null));
  // Context rows belong to both sides and carry the same text; the new
  // side is applied second so a file reads as it now stands.
  const newLines = highlightSide(newSource.join("\n"), language);
  if (newLines) newRows.forEach((row, i) => (out[row] = newLines[i] ?? null));
  return out;
}
