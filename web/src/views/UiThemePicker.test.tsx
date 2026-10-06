import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { UiThemePicker, UI_THEME_CHOICES } from "./UiThemePicker";

describe("UiThemePicker", () => {
  it("offers Midnight, Graphite, and Studio, and no longer Standard or Compact", () => {
    const html = renderToStaticMarkup(
      <UiThemePicker selected="standard" onSelect={() => {}} />,
    );
    expect(UI_THEME_CHOICES.map((choice) => choice.name)).toEqual(["Midnight", "Graphite", "Studio"]);
    for (const name of ["Midnight", "Graphite", "Studio"]) expect(html).toContain(name);
    expect(html).not.toContain("Standard");
    expect(html).not.toContain("Compact");
    expect(html).toContain("ui-theme-picker");
  });

  it("scopes each card's preview to its own theme and names its typeface", () => {
    const html = renderToStaticMarkup(
      <UiThemePicker selected="standard" onSelect={() => {}} />,
    );
    for (const theme of ["standard", "graphite", "studio"]) {
      expect(html).toContain(`data-theme="${theme}"`);
    }
    expect(html).toContain("JetBrains Mono");
    expect(html.match(/ui-theme-swatch-face">Inter</g)).toHaveLength(2);
  });

  it("marks the selected theme as active and pressed", () => {
    const html = renderToStaticMarkup(
      <UiThemePicker selected="studio" onSelect={() => {}} />,
    );
    expect(html.match(/aria-pressed="true"/g)).toHaveLength(1);
    expect(html).toMatch(/Studio<\/span><span class="theme-card-meta">Inter<em>active<\/em>/);
  });

  it("disables all choices when disabled is true", () => {
    const html = renderToStaticMarkup(
      <UiThemePicker selected="standard" disabled onSelect={() => {}} />,
    );
    expect(html.match(/disabled=""/g)).toHaveLength(UI_THEME_CHOICES.length);
  });
});
