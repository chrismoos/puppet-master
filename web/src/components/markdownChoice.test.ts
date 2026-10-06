import { describe, expect, it } from "vitest";
import { markdownChoices } from "./markdownChoice";

describe("markdown choice lists", () => {
  it("reads the marker, the options and their detail", () => {
    const choices = markdownChoices(
      [
        "# Auth",
        "",
        "<!-- pm-choice id=auth-approach select=one -->",
        "- [ ] JWT with refresh rotation",
        "  Stateless, but revocation needs a denylist.",
        "- [ ] Server-side sessions",
        "  Trivial revocation, needs sticky routing.",
        "- [ ] Other",
        "",
        "Next paragraph.",
      ].join("\n"),
    );

    expect(choices).toHaveLength(1);
    const choice = choices[0];
    expect(choice.id).toBe("auth-approach");
    expect(choice.select).toBe("one");
    // The range starts at the marker so the whole construct anchors as
    // one thing, and stops at the last option rather than the blank
    // line after it.
    expect(choice.start).toBe(3);
    expect(choice.end).toBe(8);
    expect(choice.options.map((o) => o.id)).toEqual([
      "jwt-with-refresh-rotation",
      "server-side-sessions",
      "other",
    ]);
    expect(choice.options[0].label).toBe("JWT with refresh rotation");
    expect(choice.options[0].detail).toBe("Stateless, but revocation needs a denylist.");
    expect(choice.options[0].line).toBe(4);
    expect(choice.options[2].detail).toBe("");
  });

  // The one thing that cannot be designed away: a plan's real todo list
  // must not turn into a question the reader is asked to answer.
  it("ignores a task list with no marker above it", () => {
    expect(
      markdownChoices(["- [ ] write the tests", "- [x] read the spec"].join("\n")),
    ).toEqual([]);
  });

  it("ignores a marker with no id, and one with no list under it", () => {
    expect(markdownChoices("<!-- pm-choice select=one -->\n- [ ] a")).toEqual([]);
    expect(markdownChoices("<!-- pm-choice id=lonely -->\n\nJust prose.")).toEqual([]);
  });

  it("takes select=many, and treats anything else as one", () => {
    const many = markdownChoices("<!-- pm-choice id=a select=many -->\n- [ ] x");
    expect(many[0].select).toBe("many");
    expect(markdownChoices("<!-- pm-choice id=a -->\n- [ ] x")[0].select).toBe("one");
    expect(markdownChoices("<!-- pm-choice id=a select=some -->\n- [ ] x")[0].select).toBe("one");
  });

  it("accepts quoted attribute values and extra spacing", () => {
    const choices = markdownChoices('<!--   pm-choice   id="auth 1"  select=\'many\'  -->\n- [ ] x');
    expect(choices[0].id).toBe("auth 1");
    expect(choices[0].select).toBe("many");
  });

  it("carries the tick already in the source", () => {
    const choices = markdownChoices("<!-- pm-choice id=a -->\n- [ ] no\n- [x] yes\n- [X] also");
    expect(choices[0].options.map((o) => o.checked)).toEqual([false, true, true]);
  });

  // Two options that read the same still have to be answerable apart.
  it("gives repeated and unslugabble labels distinct ids", () => {
    const choices = markdownChoices(
      ["<!-- pm-choice id=a -->", "- [ ] Same", "- [ ] Same", "- [ ] !!!", "- [ ] ???"].join("\n"),
    );
    expect(choices[0].options.map((o) => o.id)).toEqual(["same", "same-2", "option", "option-2"]);
  });

  it("keeps a blank line between options inside the list", () => {
    const choices = markdownChoices(
      ["<!-- pm-choice id=a -->", "- [ ] one", "", "- [ ] two", "  why", "", "After."].join("\n"),
    );
    expect(choices[0].options.map((o) => o.label)).toEqual(["one", "two"]);
    expect(choices[0].options[1].detail).toBe("why");
    expect(choices[0].end).toBe(5);
  });

  it("reads more than one choice in a document", () => {
    const choices = markdownChoices(
      [
        "<!-- pm-choice id=first -->",
        "- [ ] a",
        "",
        "Some prose.",
        "",
        "<!-- pm-choice id=second select=many -->",
        "- [ ] b",
        "- [ ] c",
      ].join("\n"),
    );
    expect(choices.map((c) => c.id)).toEqual(["first", "second"]);
    expect(choices[1].start).toBe(6);
    expect(choices[1].end).toBe(8);
  });

  it("stops at a bullet that is not a task item", () => {
    const choices = markdownChoices(
      ["<!-- pm-choice id=a -->", "- [ ] one", "- plain bullet", "- [ ] two"].join("\n"),
    );
    expect(choices[0].options.map((o) => o.label)).toEqual(["one"]);
    expect(choices[0].end).toBe(2);
  });
});
