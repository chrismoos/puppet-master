import { expect, test, type Page } from "./fixtures";
import { apiHeaders, logIn } from "./support";

const MONO_FACE = /^"JetBrains Mono Variable"/;
const SANS_FACE = /^"Inter Variable"/;

/// What each theme paints, keyed by the label the picker shows. `stored` is
/// the account value: Midnight is the default and stores nothing.
const THEMES = {
  Midnight: {
    stored: null, face: MONO_FACE, dark: "rgb(11, 14, 20)", light: "rgb(242, 244, 247)", casing: "uppercase",
    buttonRadius: "3px", navRadius: "4px", rowRadius: "0px", rowBar: "2px",
  },
  Graphite: {
    stored: "graphite", face: SANS_FACE, dark: "rgb(16, 17, 19)", light: "rgb(244, 245, 246)", casing: "none",
    buttonRadius: "6px", navRadius: "6px", rowRadius: "6px", rowBar: "0px",
  },
  Studio: {
    stored: "studio", face: SANS_FACE, dark: "rgb(25, 25, 28)", light: "rgb(245, 244, 241)", casing: "none",
    buttonRadius: "999px", navRadius: "10px", rowRadius: "10px", rowBar: "0px",
  },
} as const;

type ThemeName = keyof typeof THEMES;
const THEME_NAMES = Object.keys(THEMES) as ThemeName[];

const settingsUrl = () => `${process.env.PM_E2E_BASE_URL!}/api/user/settings`;

async function storedTheme(page: Page): Promise<string | null> {
  const response = await page.request.get(settingsUrl(), { headers: await apiHeaders(page) });
  return ((await response.json()) as { uiTheme: string | null }).uiTheme;
}

async function openAppearanceSettings(page: Page) {
  await page.goto(`${process.env.PM_E2E_BASE_URL!}/#/settings/appearance`);
  const appearance = page.getByRole("region", { name: "Appearance" });
  await expect(appearance).toBeVisible();
  return appearance;
}

function rootTheme(page: Page): Promise<string | null> {
  return page.evaluate(() => document.documentElement.getAttribute("data-theme"));
}

function fontFamily(page: Page, selector: string): Promise<string> {
  return page.locator(selector).first().evaluate((element) => getComputedStyle(element).fontFamily);
}

async function expectTheme(page: Page, name: ThemeName, appearance: "dark" | "light"): Promise<void> {
  const theme = THEMES[name];
  await expect(page.locator("body")).toHaveCSS("background-color", theme[appearance]);
  expect(await fontFamily(page, "body")).toMatch(theme.face);
  await expect(page.locator(".btn").first()).toHaveCSS("text-transform", theme.casing);
  expect(await rootTheme(page)).toBe(theme.stored);
  await expectShape(page, name);
}

/// Shape is part of a theme: control and Settings nav corners, page padding, the sidebar's bucket
/// label casing, and whether a session row is marked by an edge bar or by a
/// rounded fill.
async function expectShape(page: Page, name: ThemeName): Promise<void> {
  const theme = THEMES[name];
  await expect(page.locator(".btn").first()).toHaveCSS("border-top-left-radius", theme.buttonRadius);
  await expect(page.locator(".set-nav-item[aria-current=\"page\"]")).toHaveCSS("border-top-left-radius", theme.navRadius);
  await expect(page.locator(".set-main")).toHaveCSS("padding-left", name === "Studio" ? "52px" : "44px");
  await expect(page.locator(".sb-bucket-head").first()).toHaveCSS("text-transform", theme.casing);
  const row = page.locator(".sb-session-row").first();
  await expect(row).toHaveCSS("border-top-left-radius", theme.rowRadius);
  expect(await row.evaluate((element) => getComputedStyle(element, "::before").width)).toBe(theme.rowBar);

  // The primary button is an outline in Midnight and filled elsewhere.
  const primary = await page.evaluate(() => {
    const probe = document.createElement("button");
    probe.className = "btn btn-primary";
    document.body.append(probe);
    const style = getComputedStyle(probe);
    const painted = { fill: style.backgroundColor, edge: style.borderTopColor };
    probe.remove();
    return painted;
  });
  if (name === "Midnight") expect(primary.fill).toBe("rgba(0, 0, 0, 0)");
  else expect(primary.fill).toBe(primary.edge);

  // App-written labels get a raised first letter in the sentence-case themes
  // and follow their own casing in Midnight. Names a user typed never do.
  const firstLetter = (selector: string) => page.locator(selector).first()
    .evaluate((element) => getComputedStyle(element, "::first-letter").textTransform);
  expect(await firstLetter(".sb-board-link .sentence")).toBe(name === "Midnight" ? "none" : "uppercase");
  expect(await firstLetter(".conn .sentence")).toBe("uppercase");
  expect(await firstLetter(".sb-session-title")).toBe("none");
  await expect(page.locator(".sb-session-title").first()).toHaveCSS("text-transform", "none");
}

test.describe("choosing an application theme", () => {
  test.use({ colorScheme: "dark", viewport: { width: 1440, height: 900 } });

  test("restyles the whole app at once, syncs to the account, and survives a reload", async ({ page, context }) => {
    await logIn(page);
    await page.request.delete(`${settingsUrl()}/ui-theme`, { headers: await apiHeaders(page) });
    const appearance = await openAppearanceSettings(page);
    const picker = appearance.getByRole("list", { name: "Application theme" });
    const card = (name: ThemeName) => picker.getByRole("button", { name: new RegExp(name) });

    await expect(picker.getByRole("button")).toHaveCount(THEME_NAMES.length);
    await expect(picker).not.toContainText("Standard");
    await expect(picker).not.toContainText("Compact");
    await expect(card("Midnight")).toHaveAttribute("aria-pressed", "true");
    await expectTheme(page, "Midnight", "dark");

    // The bundled face has to be loadable, or the sans themes would fall
    // back to a system font and still report Inter first in the stack.
    expect(await page.evaluate(() => document.fonts.load('13px "Inter Variable"').then((faces) => faces.length)))
      .toBeGreaterThan(0);

    for (const name of ["Graphite", "Studio"] as const) {
      await card(name).click();
      await expect(card(name)).toHaveAttribute("aria-pressed", "true");
      await expect(appearance.getByRole("status")).toContainText(`${name} theme applied and synced`);
      await expectTheme(page, name, "dark");
      expect(await storedTheme(page)).toBe(THEMES[name].stored);
    }

    // The terminal theme is separate, so its preview keeps the mono face
    // under a sans interface theme.
    await page.getByRole("navigation", { name: "Settings" }).getByRole("link", { name: "Terminal theme" }).click();
    expect(await fontFamily(page, ".set-term-preview")).toMatch(MONO_FACE);
    expect(await fontFamily(page, ".set-page-head h2")).toMatch(THEMES.Studio.face);
    await page.getByRole("navigation", { name: "Settings" }).getByRole("link", { name: "Appearance" }).click();

    // Each card previews its own palette and face whichever theme is active.
    for (const name of THEME_NAMES) {
      const swatch = card(name).locator(".ui-theme-swatch");
      await expect(swatch).toHaveCSS("background-color", THEMES[name].dark);
      expect(await swatch.evaluate((element) => getComputedStyle(element).fontFamily)).toMatch(THEMES[name].face);
    }

    // The choice belongs to the account, so another tab and a reload follow it.
    const second = await context.newPage();
    await second.goto(process.env.PM_E2E_BASE_URL!);
    await expect(second.locator("body")).toHaveCSS("background-color", THEMES.Studio.dark);
    await page.reload();
    await expectTheme(page, "Studio", "dark");

    await card("Midnight").click();
    await expect(appearance.getByRole("status")).toContainText("Midnight theme restored and synced");
    await expectTheme(page, "Midnight", "dark");
    await expect(second.locator("body")).toHaveCSS("background-color", THEMES.Midnight.dark);
    expect(await storedTheme(page)).toBeNull();
    await second.close();
  });

  test("reads a stored compact choice as Graphite without rewriting it", async ({ page }) => {
    await logIn(page);
    const put = await page.request.put(`${settingsUrl()}/ui-theme`, {
      headers: { ...(await apiHeaders(page)), "content-type": "application/json" },
      data: JSON.stringify("compact"),
    });
    expect(put.status()).toBe(200);
    const appearance = await openAppearanceSettings(page);
    await page.reload();

    await expectTheme(page, "Graphite", "dark");
    const picker = appearance.getByRole("list", { name: "Application theme" });
    await expect(picker.getByRole("button", { name: /Graphite/ })).toHaveAttribute("aria-pressed", "true");
    expect(await storedTheme(page)).toBe("compact");

    await page.request.delete(`${settingsUrl()}/ui-theme`, { headers: await apiHeaders(page) });
  });
});

test.describe("with the system asking for light", () => {
  test.use({ colorScheme: "light", viewport: { width: 1440, height: 900 } });

  test("gives every theme its light palette", async ({ page }) => {
    await logIn(page);
    const appearance = await openAppearanceSettings(page);
    const picker = appearance.getByRole("list", { name: "Application theme" });
    for (const name of ["Graphite", "Studio", "Midnight"] as const) {
      await picker.getByRole("button", { name: new RegExp(name) }).click();
      await expectTheme(page, name, "light");
    }
  });
});
