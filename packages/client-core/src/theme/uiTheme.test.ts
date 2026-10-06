import { describe, expect, it } from "vitest";
import {
  parseUiTheme,
  uiThemeFromStoredValue,
  uiThemeLabel,
  USER_UI_THEME_KEY,
  UI_THEMES,
} from "./uiTheme";

describe("uiTheme", () => {
  it("exports expected key and themes", () => {
    expect(USER_UI_THEME_KEY).toBe("app.theme");
    expect(UI_THEMES).toEqual(["standard", "graphite", "studio"]);
  });

  it("parses valid choices and defaults invalid or missing values to standard", () => {
    expect(parseUiTheme(undefined)).toBe("standard");
    expect(parseUiTheme(JSON.stringify("standard"))).toBe("standard");
    expect(parseUiTheme(JSON.stringify("graphite"))).toBe("graphite");
    expect(parseUiTheme(JSON.stringify("studio"))).toBe("studio");
    expect(parseUiTheme("")).toBe("standard");
    expect(parseUiTheme("malformed")).toBe("standard");
    expect(parseUiTheme(JSON.stringify("unknown"))).toBe("standard");
    expect(parseUiTheme(JSON.stringify("midnight"))).toBe("standard");
    expect(parseUiTheme(JSON.stringify(["graphite"]))).toBe("standard");
    expect(parseUiTheme(JSON.stringify("toString"))).toBe("standard");
  });

  it("reads a stored compact choice as graphite", () => {
    expect(parseUiTheme(JSON.stringify("compact"))).toBe("graphite");
    expect(uiThemeFromStoredValue("compact")).toBe("graphite");
    expect(UI_THEMES).not.toContain("compact");
  });

  it("maps a daemon value to a theme, or to nothing when it is unknown", () => {
    expect(uiThemeFromStoredValue("studio")).toBe("studio");
    expect(uiThemeFromStoredValue("modern")).toBeUndefined();
    expect(uiThemeFromStoredValue(null)).toBeUndefined();
  });

  it("labels the stored standard value as Midnight", () => {
    expect(uiThemeLabel("standard")).toBe("Midnight");
    expect(uiThemeLabel("graphite")).toBe("Graphite");
    expect(uiThemeLabel("studio")).toBe("Studio");
  });
});
