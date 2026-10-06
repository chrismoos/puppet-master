import { expect, test } from "./fixtures";
import { logIn, openSettings } from "./support";

test("project editor sets and clears an explicit host path", async ({
  page,
  isolatedDaemon: _isolatedDaemon,
}) => {
  await logIn(page);
  await openSettings(page, "Projects");

  const project = page.locator(".manage-project").first();
  const projectId = await project.getAttribute("data-project-id");
  await project.locator(".catalog-name").click();
  const drawer = page.locator(".catalog-drawer");
  await expect(drawer.getByText("Edit project")).toBeVisible();

  const projectPath = await drawer.getByLabel("Absolute path").inputValue();
  const hostRow = drawer.locator(".catalog-host-path").first();
  const hostBadge = hostRow.locator(".catalog-host-path-head small");
  const hostInput = hostRow.locator("input");
  await expect(hostBadge).toHaveText("Inherits project path");
  await expect(hostInput).toHaveValue("");
  await expect(hostInput).toHaveAttribute("placeholder", projectPath);

  await hostInput.fill("/browser-host-path");
  await expect(hostBadge).toHaveText("Explicit path");
  await drawer.getByRole("button", { name: "Save changes" }).click();
  await expect(drawer).toBeHidden();

  await page.reload();
  await page.locator(`.manage-project[data-project-id="${projectId}"] .catalog-name`).click();
  await expect(hostInput).toHaveValue("/browser-host-path");
  await expect(hostBadge).toHaveText("Explicit path");

  await hostInput.fill("");
  await expect(hostBadge).toHaveText("Inherits project path");
  await drawer.getByRole("button", { name: "Save changes" }).click();
  await expect(drawer).toBeHidden();

  await page.locator(`.manage-project[data-project-id="${projectId}"] .catalog-name`).click();
  await expect(hostInput).toHaveValue("");
  await expect(hostBadge).toHaveText("Inherits project path");
  await expect(hostInput).toHaveAttribute("placeholder", projectPath);
});
