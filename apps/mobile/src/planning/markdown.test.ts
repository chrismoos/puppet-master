import { describe, expect, it } from "vitest";
import { Lexer, type Token, type Tokens } from "marked";

function lex(md: string): Token[] {
  return Lexer.lex(md);
}

function findToken(tokens: Token[], type: string): Token | undefined {
  for (const t of tokens) {
    if (t.type === type) return t;
    if ("tokens" in t && Array.isArray(t.tokens)) {
      const found = findToken(t.tokens, type);
      if (found) return found;
    }
    if ("items" in t && Array.isArray((t as Tokens.List).items)) {
      for (const item of (t as Tokens.List).items) {
        if (item.type === type) return item;
        const found = findToken(item.tokens, type);
        if (found) return found;
      }
    }
  }
  return undefined;
}

describe("marked lexer contract for plan Markdown", () => {
  it("parses nested unordered lists", () => {
    const tokens = lex("- A\n  - B\n    - C\n");
    const list = tokens.find((t) => t.type === "list") as Tokens.List;
    expect(list).toBeDefined();
    expect(list.ordered).toBe(false);
    expect(list.items.length).toBeGreaterThanOrEqual(1);
    const nestedList = list.items[0].tokens.find((t) => t.type === "list") as Tokens.List | undefined;
    expect(nestedList).toBeDefined();
    expect(nestedList!.items.length).toBeGreaterThanOrEqual(1);
  });

  it("parses nested ordered lists", () => {
    const tokens = lex("1. First\n   1. Sub-first\n   2. Sub-second\n2. Second\n");
    const list = tokens.find((t) => t.type === "list") as Tokens.List;
    expect(list).toBeDefined();
    expect(list.ordered).toBe(true);
    expect(list.items.length).toBe(2);
    const nestedList = list.items[0].tokens.find((t) => t.type === "list") as Tokens.List | undefined;
    expect(nestedList).toBeDefined();
    expect(nestedList!.ordered).toBe(true);
  });

  it("parses tables with headers and alignment", () => {
    const md = "| Name | Value |\n| --- | ---: |\n| foo | 42 |\n| bar | 99 |\n";
    const tokens = lex(md);
    const table = tokens.find((t) => t.type === "table") as Tokens.Table;
    expect(table).toBeDefined();
    expect(table.header.length).toBe(2);
    expect(table.rows.length).toBe(2);
    expect(table.align).toEqual([null, "right"]);
  });

  it("parses fenced code blocks with language", () => {
    const md = "```typescript\nconst x = 1;\n```\n";
    const tokens = lex(md);
    const code = tokens.find((t) => t.type === "code") as Tokens.Code;
    expect(code).toBeDefined();
    expect(code.lang).toBe("typescript");
    expect(code.text).toContain("const x = 1;");
  });

  it("parses inline links", () => {
    const md = "See [the docs](https://example.com) for details.\n";
    const tokens = lex(md);
    const link = findToken(tokens, "link") as Tokens.Link | undefined;
    expect(link).toBeDefined();
    expect(link!.href).toBe("https://example.com");
  });

  it("parses headings at all levels", () => {
    const md = "# H1\n## H2\n### H3\n#### H4\n##### H5\n###### H6\n";
    const tokens = lex(md);
    const headings = tokens.filter((t) => t.type === "heading") as Tokens.Heading[];
    expect(headings.length).toBe(6);
    expect(headings.map((h) => h.depth)).toEqual([1, 2, 3, 4, 5, 6]);
  });

  it("parses blockquotes", () => {
    const md = "> Important note\n> continued\n";
    const tokens = lex(md);
    const bq = tokens.find((t) => t.type === "blockquote") as Tokens.Blockquote;
    expect(bq).toBeDefined();
  });

  it("parses inline code spans", () => {
    const md = "Use `foo()` here.\n";
    const tokens = lex(md);
    const codespan = findToken(tokens, "codespan") as Tokens.Codespan | undefined;
    expect(codespan).toBeDefined();
    expect(codespan!.text).toBe("foo()");
  });

  it("parses mixed nested list with ordered inside unordered", () => {
    const md = "- Item A\n  1. Sub one\n  2. Sub two\n- Item B\n";
    const tokens = lex(md);
    const list = tokens.find((t) => t.type === "list") as Tokens.List;
    expect(list.ordered).toBe(false);
    const nestedOrdered = list.items[0].tokens.find((t) => t.type === "list") as Tokens.List | undefined;
    expect(nestedOrdered).toBeDefined();
    expect(nestedOrdered!.ordered).toBe(true);
  });
});
