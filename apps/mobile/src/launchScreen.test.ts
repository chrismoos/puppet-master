import { readFileSync } from "node:fs";
import { resolve } from "node:path";
import { describe, expect, it } from "vitest";
import { colors } from "./theme";

const appJson = JSON.parse(readFileSync(resolve(__dirname, "../app.json"), "utf8")) as {
  expo: { backgroundColor?: string; plugins: unknown[] };
};

function splashOptions(): { backgroundColor?: string; image?: string; dark?: { backgroundColor?: string; image?: string } } {
  const entry = appJson.expo.plugins.find((plugin) => Array.isArray(plugin) && plugin[0] === "expo-splash-screen");
  if (!Array.isArray(entry)) throw new Error("expo-splash-screen is not configured");
  return entry[1] as ReturnType<typeof splashOptions>;
}

describe("the launch screen", () => {
  it("uses the app's own background in light and dark mode, behind the app as well", () => {
    const splash = splashOptions();
    expect(splash.backgroundColor?.toLowerCase()).toBe(colors.bg);
    expect(splash.dark?.backgroundColor?.toLowerCase()).toBe(colors.bg);
    expect(appJson.expo.backgroundColor?.toLowerCase()).toBe(colors.bg);
  });

  it("shows the logo without the app icon's square tile", () => {
    expect(splashOptions().image).toBe("./assets/splash-icon.png");
    expect(readFileSync(resolve(__dirname, "../assets/splash-icon.svg"), "utf8")).not.toMatch(/<rect width="512" height="512"/);
  });
});
