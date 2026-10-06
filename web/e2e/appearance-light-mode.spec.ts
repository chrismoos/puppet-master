import { expect, test, type Page } from "./fixtures";
import { apiHeaders, logIn } from "./support";

const LIGHT_BG = "rgb(242, 244, 247)";
const DARK_BG = "rgb(11, 14, 20)";

/// Text has to contrast with whatever actually sits behind it, which is
/// rarely the element's own background: rows, chips, and banners are
/// mostly transparent over a panel. The nearest painted ancestor is the
/// colour a reader sees the text against.
const CONTRAST_PROBE = `
  const parse = (value) => {
    const parts = value.match(/[\\d.]+/g);
    if (!parts) return null;
    const alpha = parts.length > 3 ? Number(parts[3]) : 1;
    return alpha === 0 ? null : [Number(parts[0]), Number(parts[1]), Number(parts[2])];
  };
  const luminance = ([r, g, b]) => {
    const linear = [r, g, b].map((channel) => {
      const c = channel / 255;
      return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
    });
    return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
  };
  const ratio = (a, b) => {
    const x = luminance(a);
    const y = luminance(b);
    return (Math.max(x, y) + 0.05) / (Math.min(x, y) + 0.05);
  };
  const behind = (element) => {
    for (let node = element; node; node = node.parentElement) {
      const painted = parse(getComputedStyle(node).backgroundColor);
      if (painted) return painted;
    }
    return [255, 255, 255];
  };
  const results = [];
  for (const element of document.querySelectorAll(SELECTOR)) {
    const text = [...element.childNodes]
      .filter((node) => node.nodeType === Node.TEXT_NODE)
      .map((node) => node.textContent.trim())
      .join("");
    if (!text) continue;
    const box = element.getBoundingClientRect();
    if (box.width < 1 || box.height < 1) continue;
    const style = getComputedStyle(element);
    if (style.visibility === "hidden" || style.opacity === "0") continue;
    const color = parse(style.color);
    if (!color) continue;
    results.push({ text: text.slice(0, 28), ratio: Number(ratio(color, behind(element)).toFixed(2)) });
  }
  return results.sort((a, b) => a.ratio - b.ratio);
`;

/// Every visible label in the shell, whatever paints it.
const SHELL_TEXT = ".topbar *, .sidebar *, .session-home *, .btn, h1, h2, h3";

async function shellContrast(page: Page): Promise<{ text: string; ratio: number }[]> {
  return page.evaluate(
    ([selector, probe]) => new Function("SELECTOR", probe)(selector) as { text: string; ratio: number }[],
    [SHELL_TEXT, CONTRAST_PROBE] as const,
  );
}

/// The bundler shortens #ffffff to #fff, so compare on six digits.
function token(page: Page, name: string): Promise<string> {
  return page.evaluate((variable) => {
    const value = getComputedStyle(document.documentElement).getPropertyValue(variable).trim();
    return /^#[0-9a-f]{3}$/i.test(value)
      ? `#${[...value.slice(1)].map((digit) => digit + digit).join("")}`
      : value;
  }, name);
}

test.describe("with the system asking for light", () => {
  test.use({ colorScheme: "light" });

  test("paints the shell from the light palette and keeps every label legible", async ({ page }) => {
    await logIn(page);
    await expect(page.locator("body")).toHaveCSS("background-color", LIGHT_BG);
    await expect(page.locator(".sidebar").first()).not.toHaveCSS("background-color", DARK_BG);
    expect(await token(page, "--text")).toBe("#232936");
    expect(await token(page, "--accent-ink")).toBe("#ffffff");

    // The identity ships one drawing per background rather than a
    // filtered version of the other.
    await expect(page.locator(".topbar-brand .for-light-bg")).toBeVisible();
    await expect(page.locator(".topbar-brand .for-dark-bg")).toBeHidden();

    const measured = await shellContrast(page);
    // A smoke check that the probe collected a sample rather than a
    // statement about how many labels the shell has: the count moves
    // whenever a label becomes an icon. The assertion that matters is the
    // one below, and it is unchanged.
    expect(measured.length).toBeGreaterThan(15);
    expect(measured.filter((entry) => entry.ratio < 4.5)).toEqual([]);
  });
});

test.describe("with the system asking for dark", () => {
  test.use({ colorScheme: "dark" });

  test("keeps the dark palette and its labels legible", async ({ page }) => {
    await logIn(page);
    await expect(page.locator("body")).toHaveCSS("background-color", DARK_BG);
    expect(await token(page, "--text")).toBe("#c9ceda");
    expect(await token(page, "--accent-ink")).toBe("#14100a");
    await expect(page.locator(".topbar-brand .for-dark-bg")).toBeVisible();
    await expect(page.locator(".topbar-brand .for-light-bg")).toBeHidden();

    const measured = await shellContrast(page);
    // A smoke check that the probe collected a sample rather than a
    // statement about how many labels the shell has: the count moves
    // whenever a label becomes an icon. The assertion that matters is the
    // one below, and it is unchanged.
    expect(measured.length).toBeGreaterThan(15);
    expect(measured.filter((entry) => entry.ratio < 4.5)).toEqual([]);
  });
});

test.describe("choosing an appearance", () => {
  // The system asks for dark, so a light page can only be the user's own
  // choice, and returning to "system" can only mean dark again.
  test.use({ colorScheme: "dark" });

  test("overrides the system, survives a reload, and hands the page back", async ({ page, context }) => {
    await logIn(page);
    await page.request.delete(`${process.env.PM_E2E_BASE_URL!}/api/user/settings/appearance`, {
      headers: await apiHeaders(page),
    });
    await page.reload();

    const toggle = page.getByRole("button", { name: /^Appearance:/ });
    await expect(toggle).toHaveAttribute("aria-label", "Appearance: system. Switch to light.");
    await expect(page.locator("body")).toHaveCSS("background-color", DARK_BG);
    expect(await appearanceAttribute(page)).toBeNull();

    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-label", "Appearance: light. Switch to dark.");
    await expect(page.locator("body")).toHaveCSS("background-color", LIGHT_BG);
    expect(await appearanceAttribute(page)).toBe("light");

    // The choice belongs to the account, so another tab follows it.
    const second = await context.newPage();
    await second.goto(process.env.PM_E2E_BASE_URL!);
    await expect(second.locator("body")).toHaveCSS("background-color", LIGHT_BG);

    await page.reload();
    await expect(page.locator("body")).toHaveCSS("background-color", LIGHT_BG);

    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-label", "Appearance: dark. Switch to system.");
    await expect(page.locator("body")).toHaveCSS("background-color", DARK_BG);
    await expect(second.locator("body")).toHaveCSS("background-color", DARK_BG);

    // Back to no choice at all, which is the system's again.
    await toggle.click();
    await expect(toggle).toHaveAttribute("aria-label", "Appearance: system. Switch to light.");
    expect(await appearanceAttribute(page)).toBeNull();
    await expect(page.locator("body")).toHaveCSS("background-color", DARK_BG);
    await second.close();
  });
});

function appearanceAttribute(page: Page): Promise<string | null> {
  return page.evaluate(() => document.documentElement.getAttribute("data-appearance"));
}
