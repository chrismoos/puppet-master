import { describe, expect, it } from "vitest";
import {
  highlightFileRows,
  highlightSide,
  languageForPath,
  splitHighlightedLines,
} from "./diffHighlight";

describe("languageForPath", () => {
  it("maps extensions that do not name their own grammar", () => {
    expect(languageForPath("src/main.rs")).toBe("rust");
    expect(languageForPath("web/src/App.tsx")).toBe("typescript");
    expect(languageForPath("a/b/index.html")).toBe("xml");
    expect(languageForPath("deploy/values.yml")).toBe("yaml");
  });

  it("recognises files that carry no extension", () => {
    expect(languageForPath("Dockerfile")).toBe("dockerfile");
    expect(languageForPath("some/dir/Makefile")).toBe("makefile");
  });

  it("returns null rather than guessing", () => {
    // A wrong grammar colours the wrong words confidently, which reads as
    // a bug in the code being reviewed rather than in the highlighter.
    expect(languageForPath("LICENSE")).toBeNull();
    expect(languageForPath("data.bin")).toBeNull();
    expect(languageForPath(".gitignore")).toBeNull();
  });
});

describe("splitHighlightedLines", () => {
  it("gives one entry per line", () => {
    expect(splitHighlightedLines("a\nb\nc")).toEqual(["a", "b", "c"]);
  });

  it("closes a span at the line end and reopens it on the next", () => {
    // Each row is its own element, so a span left open would bleed into
    // every row after it.
    const lines = splitHighlightedLines('<span class="hljs-comment">/* one\ntwo */</span>');
    expect(lines).toEqual([
      '<span class="hljs-comment">/* one</span>',
      '<span class="hljs-comment">two */</span>',
    ]);
  });

  it("carries nested spans across a newline in order", () => {
    const lines = splitHighlightedLines(
      '<span class="a"><span class="b">x\ny</span></span>',
    );
    expect(lines).toEqual([
      '<span class="a"><span class="b">x</span></span>',
      '<span class="a"><span class="b">y</span></span>',
    ]);
  });

  it("leaves every line balanced", () => {
    const lines = splitHighlightedLines(
      '<span class="hljs-string">`a\nb\nc`</span>',
    );
    for (const line of lines) {
      const opened = (line.match(/<span\b/g) ?? []).length;
      const closed = (line.match(/<\/span>/g) ?? []).length;
      expect(closed).toBe(opened);
    }
  });
});

describe("highlightSide", () => {
  it("keeps a block comment coloured on every line it covers", () => {
    const lines = highlightSide("/* a\n b\n c */\nlet x = 1;", "javascript");
    expect(lines).not.toBeNull();
    // The middle line has nothing on it that says "comment" by itself.
    expect(lines![1]).toContain("hljs-comment");
    expect(lines![2]).toContain("hljs-comment");
    expect(lines![3]).toContain("hljs-keyword");
  });

  it("returns one entry per source line", () => {
    const source = "fn main() {\n    let x = 1;\n}";
    expect(highlightSide(source, "rust")).toHaveLength(3);
  });

  it("declines rather than guessing when there is no grammar", () => {
    expect(highlightSide("plain text", null)).toBeNull();
    expect(highlightSide("plain text", "not-a-language")).toBeNull();
  });
});

describe("highlightFileRows", () => {
  const rows = [
    { kind: "hunk", text: "@@ -1,3 +1,3 @@" },
    { kind: "context", text: "fn main() {" },
    { kind: "del", text: "    let x = 1;" },
    { kind: "add", text: "    let x = 2;" },
    { kind: "context", text: "}" },
  ];

  it("leaves a hunk header alone and colours the code", () => {
    const html = highlightFileRows("src/main.rs", rows);
    expect(html[0]).toBeNull();
    expect(html[1]).toContain("hljs-keyword");
    expect(html[2]).toContain("hljs-keyword");
    expect(html[3]).toContain("hljs-keyword");
  });

  it("returns one entry per row", () => {
    expect(highlightFileRows("src/main.rs", rows)).toHaveLength(rows.length);
  });

  it("leaves every row plain when the path has no grammar", () => {
    expect(highlightFileRows("LICENSE", rows).every((h) => h === null)).toBe(true);
  });

  it("keeps a comment opened on a deleted line coloured on the lines it covers", () => {
    // The two sides are highlighted separately, so an unterminated comment
    // on one side must not decide how the other side reads.
    const html = highlightFileRows("a.js", [
      { kind: "del", text: "/* removed" },
      { kind: "del", text: "   comment */" },
      { kind: "add", text: "const x = 1;" },
    ]);
    expect(html[0]).toContain("hljs-comment");
    expect(html[1]).toContain("hljs-comment");
    expect(html[2]).toContain("hljs-keyword");
  });
});
