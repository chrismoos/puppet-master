import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  appearanceLabel,
  nextAppearance,
  parseAppearance,
} from "@puppet-master/client-core/theme/appearance";
import { AppearanceToggle } from "./AppearanceToggle";

describe("the stored appearance choice", () => {
  it("reads the two explicit choices and nothing else", () => {
    expect(parseAppearance('"light"')).toBe("light");
    expect(parseAppearance('"dark"')).toBe("dark");
    for (const stored of [undefined, '"system"', '"Light"', "light", "null", "{}", "not json"]) {
      expect(parseAppearance(stored), String(stored)).toBeNull();
    }
  });

  it("cycles through following the system rather than stranding a user on a choice", () => {
    expect(nextAppearance(null)).toBe("light");
    expect(nextAppearance("light")).toBe("dark");
    expect(nextAppearance("dark")).toBeNull();
    expect(appearanceLabel(null)).toBe("system");
    expect(appearanceLabel("dark")).toBe("dark");
  });
});

describe("the appearance control", () => {
  it("names the state it is in and the one it moves to", () => {
    expect(renderToStaticMarkup(<AppearanceToggle appearance={null} />))
      .toContain('aria-label="Appearance: system. Switch to light."');
    expect(renderToStaticMarkup(<AppearanceToggle appearance="light" />))
      .toContain('aria-label="Appearance: light. Switch to dark."');
    expect(renderToStaticMarkup(<AppearanceToggle appearance="dark" />))
      .toContain('aria-label="Appearance: dark. Switch to system."');
  });

  it("carries its state on the element so the icon can differ", () => {
    for (const [appearance, expected] of [[null, "system"], ["light", "light"], ["dark", "dark"]] as const) {
      expect(renderToStaticMarkup(<AppearanceToggle appearance={appearance} />))
        .toContain(`data-appearance="${expected}"`);
    }
  });
});
