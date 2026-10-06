import { expect, test, type Page } from "./fixtures";
import { apiHeaders, logIn } from "./support";
import { fileURLToPath } from "node:url";

const USERNAME = "browser-e2e";
const EXTENSIONLESS_GHOSTTY_FIXTURE = fileURLToPath(new URL("./fixtures/Dracula", import.meta.url));

const BUNDLED = ["Puppet Master", "Dracula", "Gruvbox Dark", "Nord", "Solarized Dark", "Solarized Light"];

async function openTerminalTheme(page: Page): Promise<void> {
  await page.getByRole("link", { name: "Settings", exact: true }).click();
  await page.getByRole("navigation", { name: "Settings" }).getByRole("link", { name: "Terminal theme" }).click();
  await expect(page.getByRole("heading", { level: 2, name: "Terminal theme" })).toBeVisible();
}

function row(page: Page, name: string) {
  return page.getByRole("list", { name: "Terminal theme" })
    .locator(".set-theme-row", { has: page.locator("b", { hasText: new RegExp(`^${name}$`) }) });
}

/// What the bar under the preview says about the applied and previewed theme.
function previewBar(page: Page) {
  return page.locator(".set-preview-bar");
}

function termVariable(page: Page, name: string): Promise<string> {
  return page.evaluate(
    (variable) => getComputedStyle(document.documentElement).getPropertyValue(variable).trim(),
    name,
  );
}

test("the theme picker applies bundled palettes and keeps imports beside them", async ({ page }) => {
  await logIn(page);
  await page.request.delete(`${process.env.PM_E2E_BASE_URL!}/api/user/settings/terminal-theme`, {
    headers: await apiHeaders(page),
  });
  await openTerminalTheme(page);

  const list = page.getByRole("list", { name: "Terminal theme" });
  await expect(list.locator(".set-theme-row b")).toHaveText(BUNDLED);
  await expect(previewBar(page)).toHaveText("Puppet Master is active.");
  await expect(row(page, "Puppet Master")).toHaveAttribute("aria-pressed", "true");
  await expect(row(page, "Puppet Master")).toContainText("Active");
  await expect(row(page, "Solarized Light")).toContainText("Built in · Light");
  // Apply and Cancel exist only while a preview is showing.
  await expect(page.getByRole("button", { name: "Apply theme" })).toHaveCount(0);
  await expect(page.getByRole("button", { name: "Cancel", exact: true })).toHaveCount(0);

  // The list sits beside the preview, and the preview stays in view while
  // the page scrolls.
  const preview = page.locator(".set-term-preview");
  const listBox = (await list.boundingBox())!;
  const previewBox = (await preview.boundingBox())!;
  expect(previewBox.x).toBeGreaterThanOrEqual(listBox.x + listBox.width);
  expect(await page.locator(".set-term-side").evaluate((element) => getComputedStyle(element).position)).toBe("sticky");

  // A light palette is the case that proves nothing here assumes a dark
  // terminal background.
  await row(page, "Solarized Light").click();
  await expect(previewBar(page)).toContainText("Previewing Solarized Light. Your terminals still use Puppet Master.");
  await expect(row(page, "Solarized Light")).toContainText("Preview");
  const screen = page.getByLabel("Solarized Light terminal color preview").locator(".screen");
  await expect(screen).toHaveCSS("background-color", "rgb(253, 246, 227)");
  await expect(screen).toHaveCSS("color", "rgb(88, 110, 117)");

  // Canceling a preview returns to what is applied without re-choosing it.
  await row(page, "Nord").click();
  await expect(previewBar(page)).toContainText("Previewing Nord.");
  await page.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(previewBar(page)).toHaveText("Puppet Master is active.");
  await expect(page.getByRole("button", { name: "Apply theme" })).toHaveCount(0);

  await row(page, "Solarized Light").click();
  await page.getByRole("button", { name: "Apply theme" }).click();
  await expect(page.getByText(/Solarized Light applied and synced/i)).toBeVisible();
  await expect(row(page, "Solarized Light")).toContainText("Active");
  await expect(previewBar(page)).toHaveText("Solarized Light is active.");
  expect(await termVariable(page, "--term-background")).toBe("#fdf6e3");
  expect(await termVariable(page, "--term-foreground")).toBe("#586e75");

  // An imported theme joins the same list under its own name, and a
  // bundled palette stays one click away without importing again.
  await page.locator("input[type=file]").setInputFiles(EXTENSIONLESS_GHOSTTY_FIXTURE);
  await expect(previewBar(page)).toContainText("Previewing Dracula.");
  const imported = list.locator(".set-theme-row", { hasText: "Imported" });
  await expect(imported).toHaveCount(1);
  await expect(imported).toHaveAttribute("aria-pressed", "true");

  await row(page, "Gruvbox Dark").click();
  await expect(previewBar(page)).toContainText("Previewing Gruvbox Dark.");
  await expect(imported).toHaveCount(1);
  await imported.click();
  await expect(previewBar(page)).toContainText("Previewing Dracula.");
  await page.getByRole("button", { name: "Apply theme" }).click();
  await expect(page.getByText(/Dracula applied and synced/i)).toBeVisible();
  await expect(imported).toContainText("Active");
  await expect(row(page, "Solarized Light")).not.toContainText("Active");
  expect(await termVariable(page, "--term-background")).toBe("#282a36");

  await page.getByRole("button", { name: "Reset" }).click();
  await expect(page.getByText(/Built-in Puppet Master terminal theme restored/i)).toBeVisible();
  await expect(list.locator(".set-theme-row b")).toHaveText(BUNDLED);
  expect(await termVariable(page, "--term-background")).toBe("#0b0e14");
});
