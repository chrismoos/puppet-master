import { expect, test } from "./fixtures";
import { logIn } from "./support";
import type { Page } from "@playwright/test";

async function openSpawn(page: Page, title: string) {
  await logIn(page);
  await page.locator('.sb-bucket-row [title^="new session in "]').first().click();
  const pop = page.locator(".spawn-pop");
  await pop.locator('[data-chip="agent"]').click();
  await page.getByRole("menuitemradio", { name: "Codex", exact: true }).click();
  await pop.getByRole("button", { name: /permissions, profile, directory/ }).click();
  await pop.getByLabel("title (optional)", { exact: true }).fill(title);
  await pop.getByRole("button", { name: /^spawn/ }).click();
  return pop;
}

test("canceling a missing harness creates no session and runs no installer", async ({ page, isolatedDaemon }) => {
  let installations = 0;
  await page.route("**/api/harness", async (route) => {
    if (route.request().postDataJSON().install) installations += 1;
    await route.fulfill({ json: { agent: "codex", status: { state: "missing", command: "installer", output: "", error: "" } } });
  });
  const title = "cancel-harness-install";
  const pop = await openSpawn(page, title);
  await expect(pop.getByText("Codex is not installed on this worker. Install it?")).toBeVisible();
  await pop.getByRole("region", { name: "Harness installation" }).getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(pop).toBeVisible();
  await expect(pop.getByRole("button", { name: /^spawn/ })).toBeEnabled();
  expect(installations).toBe(0);
  expect(isolatedDaemon.session(title)).toBeNull();
});

test("installation shows live activity and retains failure output without launching", async ({ page, isolatedDaemon }) => {
  let state = "missing";
  let output = "";
  await page.route("**/api/harness", async (route) => {
    if (route.request().postDataJSON().install) { state = "installing"; output = "Downloading harness…"; }
    await route.fulfill({ json: { agent: "codex", status: { state, command: "installer", output, error: state === "failed" ? "Installation failed (exit status: 7)." : "" } } });
  });
  const title = "failed-harness-install";
  const pop = await openSpawn(page, title);
  await pop.getByRole("button", { name: "Install", exact: true }).click();
  await expect(pop.getByRole("progressbar", { name: "Installing Codex" })).toBeVisible();
  await expect(pop.getByLabel("Installer output")).toContainText("Downloading harness");
  output += "\nDownload complete. Installing…";
  await expect(pop.getByLabel("Installer output")).toContainText("Download complete");
  expect(isolatedDaemon.session(title)).toBeNull();
  state = "failed";
  output += "\npermission denied: cannot write executable";
  await expect(pop.getByRole("alert")).toContainText("exit status: 7");
  await expect(pop.getByLabel("Installer output")).toContainText("permission denied");
  await expect(pop.getByRole("button", { name: /^spawn/ })).toBeEnabled();
  expect(isolatedDaemon.session(title)).toBeNull();
});

test("successful installation continues the original session launch once", async ({ page, isolatedDaemon }) => {
  let state = "missing";
  let installations = 0;
  await page.route("**/api/harness", async (route) => {
    if (route.request().postDataJSON().install) { installations += 1; state = "installing"; }
    await route.fulfill({ json: { agent: "codex", status: { state, command: "installer", output: "Installing…", error: "" } } });
  });
  const title = "successful-harness-install";
  const pop = await openSpawn(page, title);
  await pop.getByRole("button", { name: "Install", exact: true }).click();
  await expect(pop.getByRole("progressbar", { name: "Installing Codex" })).toBeVisible();
  state = "ready";
  await expect(pop).toBeHidden();
  await expect.poll(() => isolatedDaemon.session(title)).not.toBeNull();
  expect(installations).toBe(1);
});

test("the full item spawn dialog preserves the form and installer error", async ({ page, isolatedDaemon }) => {
  await page.route("**/api/harness", async (route) => {
    const install = route.request().postDataJSON().install;
    await route.fulfill({ json: { agent: "claude", status: {
      state: install ? "failed" : "missing", command: "installer",
      output: install ? "curl: could not resolve host" : "",
      error: install ? "Installation failed (exit status: 6)." : "",
    } } });
  });
  await logIn(page);
  await page.getByRole("button", { name: "open board" }).first().click();
  await page.getByRole("button", { name: "new item" }).click();
  const capture = page.getByRole("dialog", { name: "capture work" });
  const title = "Item with missing harness";
  await capture.getByLabel("title").fill(title);
  await capture.getByRole("combobox", { name: "project", exact: true }).selectOption({ index: 1 });
  await capture.getByLabel("description").fill("Keep this task prompt after an installation error.");
  await capture.getByRole("button", { name: "create item" }).click();
  await expect(capture).toBeHidden();
  await page.locator(".workbench-row").filter({ hasText: title }).click();
  await page.getByRole("button", { name: "spawn session" }).click();
  const modal = page.locator(".modal");
  await modal.getByRole("button", { name: "spawn", exact: true }).click();
  await expect(modal.getByText("Claude Code is not installed on this worker. Install it?")).toBeVisible();
  await modal.getByRole("button", { name: "Install", exact: true }).click();
  await expect(modal.getByRole("alert")).toContainText("exit status: 6");
  await expect(modal.getByLabel("Installer output")).toContainText("could not resolve host");
  await expect(modal.getByLabel("title (optional)")).toHaveValue(title);
  await expect(modal.getByLabel("prompt (optional)")).toHaveValue(/Keep this task prompt/);
  expect(isolatedDaemon.session(title)).toBeNull();
});
