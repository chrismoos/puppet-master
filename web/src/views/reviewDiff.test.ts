import { describe, expect, it } from "vitest";
import {
  anchorNote,
  changesView,
  choiceBearingFiles,
  fileDefaultPreview,
  isImage,
  isMarkdown,
  nextUnviewedFile,
  parseDiff,
  draftPaths,
  liveSnapshot,
  previewExcerpt,
  renderSnapshot,
  scrollKey,
  staleAction,
  viewOptions,
  withMissingThreadFiles,
  type DiffFile,
  type PreviewDefaults,
  reviewReadError,
} from "./reviewDiff";

describe("parseDiff", () => {
  it("tracks a line number per side so a comment can anchor to either", () => {
    const files = parseDiff(
      [
        "diff --git a/src/a.rs b/src/a.rs",
        "--- a/src/a.rs",
        "+++ b/src/a.rs",
        "@@ -1,3 +1,4 @@",
        " one",
        "-two",
        "+TWO",
        "+EXTRA",
        " three",
        "",
      ].join("\n"),
    );

    expect(files).toHaveLength(1);
    const rows = files[0].rows;
    // The hunk header is a row so the reader sees where they are.
    expect(rows[0].kind).toBe("hunk");
    // Context advances both sides.
    expect(rows[1]).toMatchObject({ kind: "context", oldLine: 1, newLine: 1 });
    // A deletion has no working-tree line.
    expect(rows[2]).toMatchObject({ kind: "del", oldLine: 2, newLine: null });
    // An addition has no base line, and numbering continues.
    expect(rows[3]).toMatchObject({ kind: "add", oldLine: null, newLine: 2 });
    expect(rows[4]).toMatchObject({ kind: "add", oldLine: null, newLine: 3 });
    // Context after the change resumes from the right number on each side.
    expect(rows[5]).toMatchObject({ kind: "context", oldLine: 3, newLine: 4 });
    expect(files[0].added).toBe(2);
    expect(files[0].removed).toBe(1);
  });

  it("separates files and keeps their own counts", () => {
    const files = parseDiff(
      [
        "diff --git a/one b/one",
        "--- a/one",
        "+++ b/one",
        "@@ -1 +1 @@",
        "-a",
        "+b",
        "diff --git a/two b/two",
        "--- a/two",
        "+++ b/two",
        "@@ -0,0 +1 @@",
        "+new",
      ].join("\n"),
    );

    expect(files.map((f) => f.path)).toEqual(["one", "two"]);
    expect(files[0]).toMatchObject({ added: 1, removed: 1 });
    expect(files[1]).toMatchObject({ added: 1, removed: 0 });
  });

  it("marks a binary file rather than pretending it has rows", () => {
    const files = parseDiff(
      ["diff --git a/logo.png b/logo.png", "Binary files differ"].join("\n"),
    );
    expect(files[0].binary).toBe(true);
    expect(files[0].rows).toHaveLength(0);
  });

  it("marks a file whose base side could not be read", () => {
    const files = parseDiff(
      ["diff --git a/src/Docs.astro b/src/Docs.astro", "Unreadable: base side"].join(
        "\n",
      ),
    );
    expect(files[0].unreadable).toBe("base side");
    expect(files[0].rows).toHaveLength(0);
    expect(files[0].added).toBe(0);
  });

  it("marks a deleted file without diff rows", () => {
    const files = parseDiff(
      ["diff --git a/brand-new.rs b/brand-new.rs", "deleted file"].join("\n"),
    );
    expect(files[0].deleted).toBe(true);
    expect(files[0].rows).toHaveLength(0);
    expect(files[0].added).toBe(0);
    expect(files[0].removed).toBe(0);
  });

  it("marks a deleted file mode with diff rows", () => {
    const files = parseDiff(
      [
        "diff --git a/old.rs b/old.rs",
        "deleted file mode 100644",
        "--- a/old.rs",
        "+++ /dev/null",
        "@@ -1,2 +0,0 @@",
        "-line 1",
        "-line 2",
      ].join("\n"),
    );
    expect(files[0].deleted).toBe(true);
    expect(files[0].rows).toHaveLength(3);
    expect(files[0].added).toBe(0);
    expect(files[0].removed).toBe(2);
  });

  it("marks a new file", () => {
    const files = parseDiff(
      [
        "diff --git a/icon.png b/icon.png",
        "new file",
        "Binary files differ",
      ].join("\n"),
    );
    expect(files[0].newFile).toBe(true);
    expect(files[0].binary).toBe(true);
  });

  it("returns nothing for an empty diff", () => {
    expect(parseDiff("")).toEqual([]);
  });
});

describe("nextUnviewedFile", () => {
  const files = ["a", "b", "c", "d"];

  it("moves to the next file still needing attention", () => {
    expect(nextUnviewedFile(files, "a", new Set(["a"]))).toBe("b");
  });

  it("skips files already cleared", () => {
    expect(nextUnviewedFile(files, "a", new Set(["a", "b"]))).toBe("c");
  });

  it("does not wrap back to a file above the one just cleared", () => {
    expect(nextUnviewedFile(files, "d", new Set(["c", "d"]))).toBeNull();
    expect(nextUnviewedFile(files, "c", new Set(["a", "c"]))).toBe("d");
  });

  it("returns null once every file below is cleared", () => {
    expect(nextUnviewedFile(files, "d", new Set(files))).toBeNull();
    expect(nextUnviewedFile(files, "b", new Set(["b", "c", "d"]))).toBeNull();
  });
});

describe("anchorNote", () => {
  it("says nothing when the line has not moved", () => {
    expect(anchorNote("same", 10, 10)).toBeNull();
  });

  it("names both line numbers when the line moved", () => {
    expect(anchorNote("moved", 10, 14)).toContain("10");
    expect(anchorNote("moved", 10, 14)).toContain("14");
  });

  it("warns plainly when the commented line itself changed", () => {
    expect(anchorNote("changed", 10, 10)).toContain("changed");
  });

  it("admits when the line could not be found", () => {
    expect(anchorNote("unknown", 10, 10)).toContain("could not");
  });
});

describe("viewOptions", () => {
  it("offers only the live view when no round has completed", () => {
    expect(viewOptions([])).toEqual([{ value: "", label: "working tree (live)" }]);
  });

  it("offers a round only once both of its snapshots exist", () => {
    const halfway = viewOptions([{ rev: 1, kind: "sent" }]);
    expect(halfway.some((o) => o.value === "round:1")).toBe(false);

    const complete = viewOptions([
      { rev: 1, kind: "sent" },
      { rev: 1, kind: "received" },
    ]);
    expect(complete.some((o) => o.value === "round:1")).toBe(true);
  });

  it("labels the first delta against the base and later ones round to round", () => {
    const options = viewOptions([
      { rev: 1, kind: "sent" },
      { rev: 1, kind: "received" },
      { rev: 2, kind: "sent" },
      { rev: 2, kind: "received" },
    ]);
    expect(options.find((o) => o.value === "delta:1")?.label).toContain("base");
    expect(options.find((o) => o.value === "delta:2")?.label).toContain("Rev 1..Rev 2");
  });
});

describe("scrollKey", () => {
  it("separates the same view at a different context or layout", () => {
    expect(scrollKey("", "unified", 10)).not.toBe(scrollKey("", "unified", 20));
    expect(scrollKey("", "unified", 10)).not.toBe(scrollKey("", "side-by-side", 10));
    expect(scrollKey("round:1", "unified", 10)).not.toBe(scrollKey("", "unified", 10));
  });
});

describe("isMarkdown", () => {
  it("recognizes the markdown extensions and nothing else", () => {
    expect(isMarkdown("docs/REVIEWS.md")).toBe(true);
    expect(isMarkdown("NOTES.markdown")).toBe(true);
    expect(isMarkdown("src/main.rs")).toBe(false);
    expect(isMarkdown("mdfile")).toBe(false);
  });
});

describe("isImage", () => {
  it("recognizes image extensions including svg and raster formats", () => {
    expect(isImage("assets/logo.png")).toBe(true);
    expect(isImage("photo.JPG")).toBe(true);
    expect(isImage("avatar.jpeg")).toBe(true);
    expect(isImage("diagram.svg")).toBe(true);
    expect(isImage("anim.gif")).toBe(true);
    expect(isImage("banner.webp")).toBe(true);
    expect(isImage("favicon.ico")).toBe(true);
    expect(isImage("icon.bmp")).toBe(true);
    expect(isImage("hero.avif")).toBe(true);
    expect(isImage("src/main.rs")).toBe(false);
    expect(isImage("doc.pdf")).toBe(false);
    expect(isImage("svgfile")).toBe(false);
  });
});

const CHOICE_PLAN = [
  "# Auth",
  "",
  "<!-- pm-choice id=auth-approach select=one -->",
  "- [ ] JWT with refresh rotation",
  "- [ ] Server-side sessions",
  "",
].join("\n");

const TODO_PLAN = ["# Auth", "", "- [ ] write the migration", "- [x] read the spec", ""].join("\n");

function defaults(over: Partial<PreviewDefaults> = {}): PreviewDefaults {
  return {
    previewOff: new Set<string>(),
    markdownPreviewOn: new Set<string>(),
    choiceFiles: new Set<string>(),
    documentReview: false,
    ...over,
  };
}

describe("choiceBearingFiles", () => {
  it("names the markdown files whose rendered form asks a question", () => {
    expect(
      choiceBearingFiles({
        "plan.md": CHOICE_PLAN,
        "todo.md": TODO_PLAN,
        "notes.txt": CHOICE_PLAN,
      }),
    ).toEqual(new Set(["plan.md"]));
  });

  it("names nothing when no document was read", () => {
    expect(choiceBearingFiles({})).toEqual(new Set());
  });
});

describe("fileDefaultPreview", () => {
  it("defaults to preview unchecked (diff view) for markdown in a code review", () => {
    expect(fileDefaultPreview("README.md", defaults())).toBe(false);
    expect(fileDefaultPreview("docs/spec.markdown", defaults())).toBe(false);
  });

  it("enables preview for markdown files in markdownPreviewOn", () => {
    const d = defaults({ markdownPreviewOn: new Set(["README.md"]) });
    expect(fileDefaultPreview("README.md", d)).toBe(true);
    expect(fileDefaultPreview("docs/spec.md", d)).toBe(false);
  });

  it("enables preview for a markdown file that carries a choice list", () => {
    const d = defaults({ choiceFiles: choiceBearingFiles({ "plan.md": CHOICE_PLAN }) });
    expect(fileDefaultPreview("plan.md", d)).toBe(true);
    expect(fileDefaultPreview("README.md", d)).toBe(false);
  });

  it("enables preview for markdown in a review of one document on its own", () => {
    const d = defaults({ documentReview: true });
    expect(fileDefaultPreview("plan.md", d)).toBe(true);
    expect(fileDefaultPreview("src/main.rs", d)).toBe(false);
  });

  it("honours an explicit off over every default that would turn it on", () => {
    const d = defaults({
      previewOff: new Set(["plan.md", "icon.svg"]),
      markdownPreviewOn: new Set(["plan.md"]),
      choiceFiles: new Set(["plan.md"]),
      documentReview: true,
    });
    expect(fileDefaultPreview("plan.md", d)).toBe(false);
    expect(fileDefaultPreview("icon.svg", d)).toBe(false);
  });

  it("defaults to preview checked for image files", () => {
    expect(fileDefaultPreview("logo.png", defaults())).toBe(true);
    expect(fileDefaultPreview("icon.svg", defaults())).toBe(true);
  });

  it("returns false for non-previewable code files", () => {
    expect(fileDefaultPreview("src/main.rs", defaults())).toBe(false);
  });
});

describe("changesView", () => {
  it("points at the round that produced the reply", () => {
    expect(changesView(3)).toBe("round:3");
  });
});

describe("viewOptions with a stored view", () => {
  it("keeps the current view listed when its revision is gone", () => {
    // A round that produced no changes is not offered any more, but the
    // reader may still be pinned to it. A select whose value matches no
    // option renders blank, which looks like the picker vanished.
    const options = viewOptions([], "round:3");
    expect(options.some((o) => o.value === "round:3")).toBe(true);
    expect(options.find((o) => o.value === "round:3")?.label).toContain("no longer");
    // The live view stays first, so the fallback is still one click away.
    expect(options[0].value).toBe("");
  });

  it("does not duplicate a view that still exists", () => {
    const options = viewOptions(
      [
        { rev: 1, kind: "sent" },
        { rev: 1, kind: "received" },
      ],
      "round:1",
    );
    expect(options.filter((o) => o.value === "round:1")).toHaveLength(1);
  });

  it("adds nothing when the reader is on the live view", () => {
    expect(viewOptions([], "")).toEqual([{ value: "", label: "working tree (live)" }]);
  });
});

describe("renderSnapshot", () => {
  const response = (value: string | null) => ({
    headers: { get: (name: string) => (name === "x-review-snapshot" ? value : null) },
  });

  it("reads the snapshot the render came from", () => {
    expect(renderSnapshot(response("42"))).toBe(42n);
  });

  it("has none when the daemon named none", () => {
    expect(renderSnapshot(response(null))).toBeNull();
  });

  // A comment falls back to the tree it arrives at rather than throwing
  // the reader's comment away over a header it cannot read.
  it("has none when the header is not a snapshot id", () => {
    expect(renderSnapshot(response("not-an-id"))).toBeNull();
  });
});

describe("liveSnapshot", () => {
  const response = (value: string | null) => ({
    headers: { get: (name: string) => (name === "x-review-live" ? value : null) },
  });

  it("reads the tree the render was measured against", () => {
    expect(liveSnapshot(response("7"))).toBe(7n);
  });

  // With nothing to compare against there is nothing to ask about, so a
  // missing header leaves the page quiet rather than polling blindly.
  it("has none when the daemon named none", () => {
    expect(liveSnapshot(response(null))).toBeNull();
    expect(liveSnapshot(response("later"))).toBeNull();
  });
});

describe("draftPaths", () => {
  const threads = new Map([[4, "src/lib.rs"]]);

  it("takes the file out of a line draft's key", () => {
    expect(draftPaths({ "line:src/main.rs:12:right": "half a thought" }, threads))
      .toEqual(["src/main.rs"]);
  });

  // A path may hold colons of its own, and only the line and the side
  // are fixed at the end of the key.
  it("keeps a path that holds colons", () => {
    expect(draftPaths({ "line:odd:name.rs:3:left": "x" }, threads)).toEqual(["odd:name.rs"]);
  });

  it("takes a reply draft's file from the thread it answers", () => {
    expect(draftPaths({ "reply:4": "still writing" }, threads)).toEqual(["src/lib.rs"]);
  });

  it("ignores cleared drafts and threads it does not know", () => {
    expect(draftPaths({ "line:a.rs:1:right": "   ", "reply:99": "orphan" }, threads)).toEqual([]);
  });
});

describe("staleAction", () => {
  it("does nothing while the tree has not moved", () => {
    expect(staleAction("", [], [])).toBe("none");
    expect(staleAction("round:1", [], ["a.rs"])).toBe("none");
  });

  it("refreshes a reader who is on the working tree with nothing half-written", () => {
    expect(staleAction("", ["a.rs"], [])).toBe("refresh");
    expect(staleAction("", ["a.rs"], ["b.rs"])).toBe("refresh");
  });

  // Replacing the diff moves the line the composer is anchored to, so
  // the reader is told instead.
  it("offers rather than replaces when a changed file is being written on", () => {
    expect(staleAction("", ["a.rs"], ["a.rs"])).toBe("offer");
  });

  it("offers rather than replaces when the reader chose a stored revision", () => {
    expect(staleAction("round:1", ["a.rs"], [])).toBe("offer");
  });
});

describe("previewExcerpt", () => {
  const doc = ["# Title", "", "First paragraph.", "", "Second paragraph."].join("\n");

  it("takes the source lines the rendered block came from", () => {
    expect(previewExcerpt(doc, { start: 3, end: 3 })).toBe("First paragraph.");
    expect(previewExcerpt(doc, { start: 1, end: 3 })).toBe("# Title\n\nFirst paragraph.");
  });

  it("is empty for a document that has not arrived yet", () => {
    expect(previewExcerpt("", { start: 1, end: 1 })).toBe("");
  });
});

describe("a file the daemon renders as context only", () => {
  // Once the edit a comment asked for lands, the file can differ from
  // the base nowhere at all. The daemon still sends it, carrying the
  // lines the comment sits on, so the conversation stays readable.
  const contextOnly = [
    "diff --git a/src/lib.rs b/src/lib.rs",
    "--- a/src/lib.rs",
    "+++ b/src/lib.rs",
    "@@ -1,3 +1,3 @@",
    " one",
    " two",
    " three",
    "",
  ].join("\n");

  it("parses as a file with rows and no change on either side", () => {
    const [file] = parseDiff(contextOnly);

    expect(file.path).toBe("src/lib.rs");
    expect(file.added).toBe(0);
    expect(file.removed).toBe(0);
    expect(file.rows.filter((r) => r.kind === "context")).toHaveLength(3);
    // Numbered, so a thread anchored in here still finds its line.
    expect(file.rows.find((r) => r.text === "two")?.newLine).toBe(2);
  });
});

describe("withMissingThreadFiles", () => {
  const file = (path: string): DiffFile => ({
    path,
    rows: [],
    added: 3,
    removed: 1,
    binary: false,
    deleted: false,
    unreadable: null,
  });

  it("keeps the diff untouched before a diff response has arrived", () => {
    expect(withMissingThreadFiles([], ["gamma.txt"], false)).toEqual([]);
  });

  it("adds a deleted placeholder for a thread the diff does not carry", () => {
    const out = withMissingThreadFiles([file("alpha.txt")], ["alpha.txt", "gone.txt"], true);
    expect(out.map((f) => f.path)).toEqual(["alpha.txt", "gone.txt"]);
    expect(out[1].deleted).toBe(true);
    expect(out[0].deleted).toBe(false);
  });

  it("adds one placeholder however many threads name the same file", () => {
    const out = withMissingThreadFiles([], ["gone.txt", "gone.txt"], true);
    expect(out).toHaveLength(1);
  });

  it("does not mark a file deleted just because the diff carries it", () => {
    const out = withMissingThreadFiles([file("gamma.txt")], ["gamma.txt"], true);
    expect(out).toHaveLength(1);
    expect(out[0].deleted).toBe(false);
  });
});

describe("reviewReadError", () => {
  const response = (body: unknown, status = 404): Response =>
    ({ status, json: async () => body }) as unknown as Response;

  it("reports the daemon's own reason", async () => {
    const res = response({ error: '"/tmp/gone" does not appear to be a git repository' });
    await expect(reviewReadError(res, 34)).resolves.toBe(
      '"/tmp/gone" does not appear to be a git repository',
    );
  });

  it("falls back to the status when the body carries no reason", async () => {
    await expect(reviewReadError(response({}), 34)).resolves.toBe(
      "review 34 could not be read (404)",
    );
  });

  it("falls back when the body is not json", async () => {
    const res = { status: 500, json: async () => { throw new Error("not json"); } } as unknown as Response;
    await expect(reviewReadError(res, 34)).resolves.toBe("review 34 could not be read (500)");
  });

  it("ignores a blank reason", async () => {
    await expect(reviewReadError(response({ error: "   " }), 7)).resolves.toBe(
      "review 7 could not be read (404)",
    );
  });
});
