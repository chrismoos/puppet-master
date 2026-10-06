import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { Markdown, markdownAnchors, markdownBlocks } from "../components/Markdown";

// The preview used to render one source line at a time so that every line
// could carry a comment anchor. That collapsed blank lines to nothing and
// split anything spanning lines, so these cover the whole-document render
// that replaced it.
const DOC = [
  "# Title",
  "",
  "First paragraph.",
  "",
  "Second paragraph.",
  "",
  "- one",
  "- two",
  "",
  "```rust",
  "fn main() {}",
  "let x = 1;",
  "```",
].join("\n");

const render = (text: string) =>
  renderToStaticMarkup(<Markdown text={text} onPmLink={() => undefined} />);

describe("review markdown preview", () => {
  it("keeps consecutive list items in one list", () => {
    const html = render(DOC);
    expect(html).toContain("<li>one</li><li>two</li>");
    expect((html.match(/<ul>/g) ?? []).length).toBe(1);
  });

  it("keeps a fenced block whole instead of splitting it per line", () => {
    const html = render(DOC);
    expect(html).toContain("fn main() {}\nlet x = 1;");
    expect((html.match(/<pre>/g) ?? []).length).toBe(1);
  });

  it("separates paragraphs rather than emitting an empty element per blank line", () => {
    const html = render(DOC);
    expect(html).toContain("<p>First paragraph.</p>");
    expect(html).toContain("<p>Second paragraph.</p>");
    // A blank line must not survive as its own empty block, which is what
    // made blank lines shorter than the lines around them.
    expect(html).not.toContain("<p></p>");
    expect(html).not.toContain("<p> </p>");
  });

  it("renders a line-by-line split incorrectly, which is why it was replaced", () => {
    const perLine = DOC.split("\n").map((line) => render(line || " ")).join("");
    expect((perLine.match(/<ul>/g) ?? []).length).toBe(2);
    expect(perLine).not.toContain("fn main() {}\nlet x = 1;");
  });
});

describe("preview block anchors", () => {
  it("reports the source line range each block came from", () => {
    const blocks = markdownBlocks(DOC);
    expect(blocks.map((b) => [b.kind, b.start, b.end])).toEqual([
      ["heading", 1, 1],
      ["paragraph", 3, 3],
      ["paragraph", 5, 5],
      ["list", 7, 8],
      ["fence", 10, 13],
    ]);
  });

  it("covers a multi-line block with one range rather than one per line", () => {
    const fence = markdownBlocks(DOC).find((b) => b.kind === "fence")!;
    expect(fence.end - fence.start).toBeGreaterThan(0);
    expect(fence.lines).toEqual(["fn main() {}", "let x = 1;"]);
  });

  it("hands each anchorable region to renderBlock with its range", () => {
    const seen: Array<[number, number]> = [];
    renderToStaticMarkup(
      <Markdown
        text={DOC}
        onPmLink={() => undefined}
        renderBlock={(block, range) => {
          seen.push([range.start, range.end]);
          return block;
        }}
      />,
    );
    // The list at lines 7-8 arrives as two anchors rather than one, so a
    // comment can land on a single bullet. The fence stays one anchor: a
    // code block is read as a unit and its lines are not separate points.
    expect(seen).toEqual([[1, 1], [3, 3], [5, 5], [7, 7], [8, 8], [10, 13]]);
  });

  it("leaves the markup alone when no renderBlock is given", () => {
    const plain = render(DOC);
    const wrapped = renderToStaticMarkup(
      <Markdown text={DOC} onPmLink={() => undefined} renderBlock={(b) => b} />,
    );
    expect(wrapped).toBe(plain);
  });
});

describe("list items anchor individually", () => {
  const LIST = ["# Title", "", "- alpha", "- beta", "- gamma", "", "After."].join("\n");

  function anchoredRanges(text: string): { start: number; end: number }[] {
    const ranges: { start: number; end: number }[] = [];
    renderToStaticMarkup(
      <Markdown
        text={text}
        onPmLink={() => undefined}
        renderBlock={(block, range) => {
          ranges.push(range);
          return block;
        }}
      />,
    );
    return ranges;
  }

  it("gives every bullet its own anchor on its own source line", () => {
    // A comment on the third bullet has to mean the third bullet, not the
    // list; anchoring the whole list was the bug.
    expect(anchoredRanges(LIST)).toEqual([
      { start: 1, end: 1 },
      { start: 3, end: 3 },
      { start: 4, end: 4 },
      { start: 5, end: 5 },
      { start: 7, end: 7 },
    ]);
  });

  it("never anchors the list as a whole as well", () => {
    // A range covering several bullets would put two anchors over the same
    // lines, and a comment could land on either.
    const spans = anchoredRanges(LIST).filter((r) => r.end > r.start);
    expect(spans).toEqual([]);
  });

  it("keeps the items inside a real list", () => {
    // The wrapper belongs inside the <li>: a div between <ul> and <li> is
    // not a list to markup or to a screen reader.
    const html = renderToStaticMarkup(
      <Markdown
        text={LIST}
        onPmLink={() => undefined}
        renderBlock={(block) => <div className="wrap">{block}</div>}
      />,
    );
    expect(html).toContain("<ul><li><div class=\"wrap\">alpha</div></li>");
    expect(html).not.toContain("<ul><div");
  });

  it("anchors a numbered list the same way", () => {
    const ordered = ["1. one", "2. two"].join("\n");
    expect(anchoredRanges(ordered)).toEqual([
      { start: 1, end: 1 },
      { start: 2, end: 2 },
    ]);
  });

  it("anchors a nested bullet to its own line too", () => {
    const nested = ["- outer", "  - inner", "- last"].join("\n");
    expect(anchoredRanges(nested)).toEqual([
      { start: 1, end: 1 },
      { start: 2, end: 2 },
      { start: 3, end: 3 },
    ]);
  });

  it("renders nested bullets inside their parent list items", () => {
    const nested = ["- outer", "  - inner", "    1. deep", "- last"].join("\n");
    const html = render(nested);
    expect(html).toContain(
      "<ul><li>outer<ul><li>inner<ol><li>deep</li></ol></li></ul></li><li>last</li></ul>",
    );
  });

  it("keeps nested anchor wrappers inside valid list markup", () => {
    const nested = ["- outer", "  - inner", "- last"].join("\n");
    const html = renderToStaticMarkup(
      <Markdown
        text={nested}
        onPmLink={() => undefined}
        renderBlock={(block) => <div className="wrap">{block}</div>}
      />,
    );
    expect(html).toContain(
      '<ul><li><div class="wrap">outer</div><ul><li><div class="wrap">inner</div></li></ul></li>',
    );
    expect(html).not.toContain("<ul><div");
  });

  it("keeps loose ordered list items in a single list across blank lines", () => {
    const markdown = ["1. first", "", "2. second", "", "3. third"].join("\n");
    const html = render(markdown);
    expect(html).toContain("<ol><li>first</li><li>second</li><li>third</li></ol>");
  });

  it("joins continuation lines under list items into the item body", () => {
    const markdown = [
      "1. **Shared blast radius.** Ingest floods share the Tomcat pool,",
      "   Redis and TLS cert with the dashboard API.",
      "",
      "2. **Per-IP throttles.** Dependent on trusted proxies.",
    ].join("\n");
    const html = render(markdown);
    expect(html).toContain(
      "<ol><li><strong>Shared blast radius.</strong> Ingest floods share the Tomcat pool, Redis and TLS cert with the dashboard API.</li><li><strong>Per-IP throttles.</strong> Dependent on trusted proxies.</li></ol>",
    );
  });

  it("handles bullet lists with multiline continuation lines", () => {
    const markdown = [
      "- `navigator.webdriver`, permissions API,",
      "  WebGPU adapter info, canvas,",
      "  deviceMemory, connection.",
      "- Storage: `sessionStorage.bladerun_sid`.",
    ].join("\n");
    const html = render(markdown);
    expect((html.match(/<ul>/g) ?? []).length).toBe(1);
    expect(html).toContain(
      "<ul><li><code>navigator.webdriver</code>, permissions API, WebGPU adapter info, canvas, deviceMemory, connection.</li><li>Storage: <code>sessionStorage.bladerun_sid</code>.</li></ul>",
    );
  });

  it("sets the start attribute on ordered lists starting at a number other than 1", () => {
    const markdown = ["3. third", "4. fourth"].join("\n");
    const html = render(markdown);
    expect(html).toContain('<ol start="3"><li>third</li><li>fourth</li></ol>');
  });

  it("does not set start attribute when ordered list starts at 1", () => {
    const markdown = ["1. first", "2. second"].join("\n");
    const html = render(markdown);
    expect(html).toContain("<ol><li>first</li><li>second</li></ol>");
  });

  it("anchors loose lists and multiline items to their bullet line", () => {
    const markdown = ["1. first", "   continued", "", "2. second"].join("\n");
    expect(anchoredRanges(markdown)).toEqual([
      { start: 1, end: 1 },
      { start: 4, end: 4 },
    ]);
  });
});

describe("tables", () => {
  const TABLE = [
    "| Name | Result | Notes |",
    "| :--- | :----: | ----: |",
    "| parser | **pass** | `a|b` |",
    "| links | [item](pm:item/12) | escaped \\| pipe |",
  ].join("\n");

  it("renders a header, body rows, inline markdown, and alignment", () => {
    const html = render(TABLE);
    expect(html).toContain("<table><thead><tr>");
    expect(html).toContain('<th style="text-align:left">Name</th>');
    expect(html).toContain('<th style="text-align:center">Result</th>');
    expect(html).toContain('<th style="text-align:right">Notes</th>');
    expect(html).toContain('<td style="text-align:center"><strong>pass</strong></td>');
    expect(html).toContain('<td style="text-align:right"><code>a|b</code></td>');
    expect(html).toContain('<td style="text-align:right">escaped | pipe</td>');
  });

  it("anchors the table as one valid block", () => {
    expect(markdownAnchors(TABLE)).toEqual([{ start: 1, end: 4 }]);
    const seen: { start: number; end: number }[] = [];
    renderToStaticMarkup(
      <Markdown
        text={TABLE}
        onPmLink={() => undefined}
        renderBlock={(block, range) => {
          seen.push(range);
          return <div className="wrap">{block}</div>;
        }}
      />,
    );
    expect(seen).toEqual([{ start: 1, end: 4 }]);
  });

  it("does not treat a pipe row without a delimiter as a table", () => {
    expect(render("alpha | beta\nplain text")).toContain("<p>alpha | beta plain text</p>");
  });
});

describe("markdownAnchors agrees with what the renderer emits", () => {
  const DOCS = [
    ["# Title", "", "- alpha", "- beta", "", "Tail."].join("\n"),
    ["1. one", "2. two", "3. three"].join("\n"),
    ["Para one.", "", "```js", "const x = 1;", "```", "", "- only"].join("\n"),
    ["- outer", "  - inner", "", "# End"].join("\n"),
    ["1. first", "   continued", "", "2. second"].join("\n"),
    ["- alpha", "  detail line", "", "- beta"].join("\n"),
    ["3. third", "", "4. fourth"].join("\n"),
  ];

  it("returns exactly the ranges renderBlock is called with", () => {
    // Two definitions of "an anchor" is how a comment made on one bullet
    // reappears under a different one: the renderer places it by one rule
    // and the thread map finds it by another.
    for (const doc of DOCS) {
      const emitted: { start: number; end: number }[] = [];
      renderToStaticMarkup(
        <Markdown
          text={doc}
          onPmLink={() => undefined}
          renderBlock={(block, range) => {
            emitted.push({ start: range.start, end: range.end });
            return block;
          }}
        />,
      );
      expect(markdownAnchors(doc)).toEqual(emitted);
    }
  });
});

describe("a marked choice list is one anchorable block", () => {
  const CHOICE = [
    "Pick one.",
    "",
    "<!-- pm-choice id=auth-approach select=one -->",
    "- [ ] JWT with refresh rotation",
    "  Stateless, but revocation needs a denylist.",
    "- [ ] Server-side sessions",
    "",
    "After.",
  ].join("\n");

  it("anchors the marker and its options together rather than per bullet", () => {
    const blocks = markdownBlocks(CHOICE);
    const choice = blocks.find((b) => b.kind === "choice");
    expect(choice).toBeDefined();
    expect({ start: choice!.start, end: choice!.end }).toEqual({ start: 3, end: 6 });
    // One anchor for the whole question: an answer belongs to the
    // question, not to whichever bullet the reader happened to click.
    expect(markdownAnchors(CHOICE)).toEqual([
      { start: 1, end: 1 },
      { start: 3, end: 6 },
      { start: 8, end: 8 },
    ]);
  });

  it("hands the parsed choice to renderBlock so it can be a control", () => {
    const seen: (string | undefined)[] = [];
    renderToStaticMarkup(
      <Markdown
        text={CHOICE}
        onPmLink={() => undefined}
        renderBlock={(block, _range, choice) => {
          seen.push(choice?.id);
          return block;
        }}
      />,
    );
    expect(seen).toEqual([undefined, "auth-approach", undefined]);
  });

  // The whole reason the marker exists.
  it("leaves an unmarked task list exactly as it renders today", () => {
    const todo = ["- [ ] write the tests", "- [x] read the spec"].join("\n");
    const html = render(todo);
    expect(html).toContain("<li>[ ] write the tests</li><li>[x] read the spec</li>");
    expect(markdownAnchors(todo)).toEqual([
      { start: 1, end: 1 },
      { start: 2, end: 2 },
    ]);
    const seen: (string | undefined)[] = [];
    renderToStaticMarkup(
      <Markdown
        text={todo}
        onPmLink={() => undefined}
        renderBlock={(block, _range, choice) => {
          seen.push(choice?.id);
          return block;
        }}
      />,
    );
    expect(seen).toEqual([undefined, undefined]);
  });
});
