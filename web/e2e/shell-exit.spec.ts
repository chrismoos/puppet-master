import { expect, test, type Locator, type Page } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";

async function openSession(page: Page): Promise<void> {
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await expect(page.locator(".terminal-tabs")).toBeVisible();
}

async function createShell(page: Page): Promise<void> {
  const tabs = page.locator(".terminal-tab");
  const before = await tabs.count();
  await page.getByRole("button", { name: "+ Shell", exact: true }).click();
  await expect(tabs).toHaveCount(before + 1);
  await expect(tabs.last()).toHaveClass(/active/);
  await expectTerminalRevealed(page, "t:");
}

async function typeInVisibleTerminal(page: Page, command: string, host?: Locator): Promise<void> {
  const textarea = (host ?? page.locator("body")).locator(".xterm-helper-textarea:visible");
  await textarea.focus();
  await page.keyboard.type(command);
  await page.keyboard.press("Enter");
}

test.describe.serial("clean shell exit", () => {
  test("active and background shell exits remove only their shell tabs", async ({ page }) => {
    await openSession(page);
    const sessionRows = await page.locator(".sb-session").count();

    await createShell(page);
    await typeInVisibleTerminal(page, "exit");
    await expect(page.locator(".terminal-tab")).toHaveCount(0);
    await expect(page.getByRole("button", { name: "agent", exact: true })).toHaveClass(/active/);

    await createShell(page);
    await typeInVisibleTerminal(page, "sleep 0.2; exit");
    await page.getByRole("button", { name: "agent", exact: true }).click();
    await expect(page.locator(".terminal-tab")).toHaveCount(0);
    await expect(page.getByRole("button", { name: "agent", exact: true })).toHaveClass(/active/);
    await expect(page.locator(".sb-session")).toHaveCount(sessionRows);
    await expect(page.locator(".pane-title")).toBeVisible();
  });

  test("a shell shared with a workspace collapses its pane and leaves no stale selector", async ({ page }) => {
    await openSession(page);
    await createShell(page);

    await page.getByTitle("new workspace").click();
    await page.getByRole("button", { name: "create workspace" }).click();
    await expect(page.locator(".workspace-pane")).toHaveCount(1);
    await page.getByTitle("split right").click();
    await expect(page.locator(".workspace-pane")).toHaveCount(2);

    const secondSelector = page.getByLabel("terminal shown in pane").nth(1);
    const shellValue = await secondSelector.locator("option").evaluateAll((options) => {
      const shell = options.find((option) => option.getAttribute("value") && !option.textContent?.includes("agent"));
      if (!(shell instanceof HTMLOptionElement)) throw new Error("missing shell option");
      return shell.value;
    });
    await secondSelector.selectOption(shellValue);
    await typeInVisibleTerminal(page, "exit", page.locator(".workspace-pane").nth(1));

    await expect(page.locator(".workspace-pane")).toHaveCount(1);
    await expect(page.getByLabel("terminal shown in pane").locator(`option[value="${shellValue}"]`)).toHaveCount(0);
    await page.locator(".sb-session.is-selected").click();
    await expect(page.locator(".terminal-tab")).toHaveCount(0);
    await expect(page.getByRole("button", { name: "agent", exact: true })).toBeVisible();
  });
});
