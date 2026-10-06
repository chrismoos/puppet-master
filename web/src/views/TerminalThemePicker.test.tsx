import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { BUILTIN_TERMINAL_THEMES, findBuiltinTerminalTheme } from "@puppet-master/client-core/theme/builtinTerminalThemes";
import { BUILTIN_TERMINAL_THEME, type TerminalTheme } from "@puppet-master/client-core/theme/terminalTheme";
import {
  IMPORTED_CHOICE_KEY,
  TerminalThemePicker,
  activeChoiceKey,
  themeChoices,
} from "./TerminalThemePicker";

const IMPORT: TerminalTheme = {
  ...BUILTIN_TERMINAL_THEME,
  name: "Ghostty Midnight",
  colors: { ...BUILTIN_TERMINAL_THEME.colors, background: "#050505" },
};

describe("terminal theme choices", () => {
  it("offers every bundled palette, and nothing else while one is applied", () => {
    const choices = themeChoices(BUILTIN_TERMINAL_THEME, null);
    expect(choices.map((choice) => choice.theme.name))
      .toEqual(BUILTIN_TERMINAL_THEMES.map((theme) => theme.name));
    expect(choices.every((choice) => choice.origin === "built-in")).toBe(true);
  });

  it("keeps an applied import in the list under its own name", () => {
    const choices = themeChoices(IMPORT, null);
    const custom = choices.at(-1)!;
    expect(custom).toMatchObject({ key: IMPORTED_CHOICE_KEY, origin: "imported" });
    expect(custom.theme.name).toBe("Ghostty Midnight");
    expect(activeChoiceKey(choices, IMPORT)).toBe(IMPORTED_CHOICE_KEY);
  });

  it("keeps this session's import selectable after a built-in is previewed", () => {
    const choices = themeChoices(BUILTIN_TERMINAL_THEME, IMPORT);
    expect(choices.filter((choice) => choice.origin === "imported").map((choice) => choice.theme.name))
      .toEqual(["Ghostty Midnight"]);
    expect(activeChoiceKey(choices, BUILTIN_TERMINAL_THEME)).toBe("builtin:Puppet Master");
  });

  it("resolves the active choice by palette, not by the name a theme carries", () => {
    const dracula = findBuiltinTerminalTheme("Dracula")!;
    const renamed = { ...dracula, name: "my dracula" };
    const choices = themeChoices(renamed, null);
    expect(activeChoiceKey(choices, renamed)).toBe("builtin:Dracula");
    expect(choices.some((choice) => choice.origin === "imported")).toBe(false);
  });

  it("marks the imported card active when its palette matches a bundled one", () => {
    const dracula = findBuiltinTerminalTheme("Dracula")!;
    const asImported = { ...dracula, name: "Dracula" };
    const choices = themeChoices(asImported, asImported);
    expect(choices.filter((choice) => choice.theme.name === "Dracula")).toHaveLength(2);
    expect(activeChoiceKey(choices, asImported)).toBe(IMPORTED_CHOICE_KEY);
    expect(activeChoiceKey(choices, dracula)).toBe(IMPORTED_CHOICE_KEY);
    expect(activeChoiceKey(choices, { ...dracula, name: "Something else" })).toBe("builtin:Dracula");
  });

  it("labels a bundled palette's appearance so a light one is findable", () => {
    const choices = themeChoices(BUILTIN_TERMINAL_THEME, null);
    const byName = new Map(choices.map((choice) => [choice.theme.name, choice.appearance]));
    expect(byName.get("Solarized Light")).toBe("light");
    expect(byName.get("Dracula")).toBe("dark");
  });
});

describe("terminal theme picker rendering", () => {
  const render = (selectedKey: string, activeKey: string) =>
    renderToStaticMarkup(
      <TerminalThemePicker
        choices={themeChoices(BUILTIN_TERMINAL_THEME, IMPORT)}
        selectedKey={selectedKey}
        activeKey={activeKey}
        disabled={false}
        onSelect={() => {}}
      />,
    );

  it("presses only the selected row and marks the applied one active", () => {
    const html = render("builtin:Puppet Master", "builtin:Puppet Master");
    expect(html.match(/aria-pressed="true"/g)).toHaveLength(1);
    expect(html.match(/>Active</g)).toHaveLength(1);
    expect(html).not.toContain(">Preview<");
  });

  it("separates the previewed row from the applied one", () => {
    const html = render(IMPORTED_CHOICE_KEY, "builtin:Puppet Master");
    expect(html.match(/>Preview</g)).toHaveLength(1);
    expect(html.match(/>Active</g)).toHaveLength(1);
    expect(html).toContain("Imported · Dark");
    expect(html).toContain("Built in · Light");
  });

  it("paints each row from its own palette", () => {
    const html = render("builtin:Dracula", "builtin:Dracula");
    expect(html).toContain("background:#282a36");
    expect(html).toContain("background:#fdf6e3");
  });

  it("disables every row while a save is in flight", () => {
    const html = renderToStaticMarkup(
      <TerminalThemePicker
        choices={themeChoices(BUILTIN_TERMINAL_THEME, null)}
        selectedKey="builtin:Nord"
        activeKey="builtin:Puppet Master"
        disabled
        onSelect={() => {}}
      />,
    );
    expect(html.match(/disabled=""/g)).toHaveLength(BUILTIN_TERMINAL_THEMES.length);
  });
});
