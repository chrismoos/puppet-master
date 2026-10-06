import { expect, test } from "./fixtures";
import { apiHeaders, logIn, openSettings } from "./support";
import type { Page } from "@playwright/test";

/** The bucket ＋ is the only spawn entry; the project is picked on its chip. */
async function openSpawn(page: Page, projectName?: string) {
  await page.locator('.sb-bucket-row [title^="new session in "]').first().click();
  const pop = page.locator(".spawn-pop");
  if (projectName !== undefined) {
    await pop.locator('[data-chip="project"]').click();
    await page.getByRole("menuitemradio", { name: projectName, exact: true }).click();
  }
  return pop;
}

test("spawn popover shows project and bucket agent defaults plus explicit override", async ({
  page,
  isolatedDaemon: _isolatedDaemon,
}) => {
  await logIn(page);
  await openSettings(page, "Projects");
  await page.getByRole("tab", { name: /Buckets/ }).click();

  const bucket = page.locator(".manage-bucket").first();
  await bucket.locator(".catalog-name").click();
  const drawer = page.locator(".catalog-drawer");
  await drawer.getByLabel("Default Agent").selectOption("codex");
  await drawer.getByRole("button", { name: "Save changes" }).click();
  await expect(drawer).toBeHidden();

  await page.getByRole("tab", { name: /Projects/ }).click();
  const project = page.locator(".manage-project").first();
  const projectName = (await project.locator(".catalog-name b").textContent())!;
  await project.locator(".catalog-name").click();
  await drawer.getByLabel("Default Agent").selectOption("claude");
  await drawer.getByRole("button", { name: "Save changes" }).click();
  await expect(drawer).toBeHidden();

  await page.getByRole("button", { name: "Back to sessions" }).click();
  await openSpawn(page, projectName);
  const pop = page.locator(".spawn-pop");
  const agentChip = pop.locator('[data-chip="agent"]');
  await expect(agentChip).toHaveText("Claude");
  await expect(agentChip).toHaveClass(/is-default/);
  await agentChip.click();
  await expect(page.getByRole("menuitemradio", { name: "Claude — project default" }))
    .toHaveAttribute("aria-checked", "true");
  // The rows come from the shared agent table, so every agent a spawn can
  // choose is offered here.
  for (const label of ["Claude", "Codex", "Gemini", "OpenCode", "Antigravity"]) {
    await expect(page.getByRole("menuitemradio", { name: label, exact: true }))
      .toBeVisible();
  }
  await page.getByRole("menuitemradio", { name: "Gemini", exact: true }).click();
  await expect(agentChip).toHaveText("Gemini");
  await expect(agentChip).not.toHaveClass(/is-default/);
  await agentChip.click();
  await page.getByRole("menuitemradio", { name: "Codex", exact: true }).click();
  await expect(agentChip).toHaveText("Codex");
  await expect(agentChip).not.toHaveClass(/is-default/);
  await page.keyboard.press("Escape");
  await expect(pop).toBeHidden();

  await openSettings(page, "Projects");
  await page.getByRole("tab", { name: /Projects/ }).click();
  await page.locator(".manage-project").first().locator(".catalog-name").click();
  await drawer.getByLabel("Default Agent").selectOption("");
  await drawer.getByRole("button", { name: "Save changes" }).click();
  await page.getByRole("button", { name: "Back to sessions" }).click();
  await openSpawn(page, projectName);
  await expect(pop.locator('[data-chip="agent"]')).toHaveText("Codex");
  await expect(pop.locator('[data-chip="agent"]')).toHaveClass(/is-default/);
});

test("the bucket (+) commits the picked project's working directory immediately", async ({ page }) => {
  await logIn(page);
  await openSettings(page, "Projects");

  const project = page.locator(".manage-project").first();
  const projectName = await project.locator(".catalog-name b").textContent();
  const projectPath = await project.locator(".project-path").textContent();
  expect(projectName).not.toBeNull();
  expect(projectPath).not.toBeNull();

  await page.getByRole("button", { name: "Back to sessions" }).click();
  await page.evaluate(() => {
    const capture = window as Window & { __initialSpawnCwd?: string };
    delete capture.__initialSpawnCwd;
    const observer = new MutationObserver(() => {
      const input = document.querySelector<HTMLInputElement>(".spawn-pop .dirpicker input");
      if (!input) return;
      capture.__initialSpawnCwd = input.value;
      observer.disconnect();
    });
    observer.observe(document.body, { childList: true, subtree: true });
  });

  await openSpawn(page, projectName);

  const pop = page.locator(".spawn-pop");
  await expect(pop.locator('[data-chip="project"]')).toHaveText(projectName!);
  await pop.getByRole("button", { name: /permissions, profile, directory/ }).click();
  await expect(pop.locator(".dirpicker input")).toHaveValue(projectPath!);
  await expect.poll(() => page.evaluate(() =>
    (window as Window & { __initialSpawnCwd?: string }).__initialSpawnCwd,
  )).toBe(projectPath!);
});

test("remote cwd override directs one Supervisor spawn and leaves the project default", async ({
  page,
  isolatedDaemon,
}) => {
  await logIn(page);
  const enrollment = await page.request.post(`${process.env.PM_E2E_BASE_URL}/api/workers/enroll`, {
    headers: await apiHeaders(page),
    data: { label: "remote-e2e" },
  });
  expect(enrollment.ok()).toBeTruthy();
  const { token } = await enrollment.json() as { token: string };
  await isolatedDaemon.startRemoteWorker(token);

  await openSettings(page, "Workers");
  const remoteHost = page.locator(".worker-row").filter({ hasText: "remote-e2e" });
  await expect(remoteHost.locator(".worker-status")).toHaveText("Online");
  await expect.poll(isolatedDaemon.workerDefaultRoot).toBe("~/");

  await page.getByRole("link", { name: "projects" }).click();
  await page.getByRole("tab", { name: /Buckets/ }).click();
  const bucket = page.locator(".manage-bucket").first();
  await bucket.locator(".catalog-name").click();
  const bucketEditor = page.locator(".catalog-drawer");
  await bucketEditor.getByText("remote-e2e").click();
  await bucketEditor.getByRole("button", { name: "Save changes" }).click();
  await expect(bucketEditor).toBeHidden();

  await page.getByRole("tab", { name: /Projects/ }).click();
  const project = page.locator(".manage-project").first();
  const projectName = (await project.locator(".catalog-name b").textContent())!;
  const projectPath = (await project.locator(".project-path").textContent())!;
  await project.locator(".catalog-name").click();
  const projectEditor = page.locator(".catalog-drawer");
  await projectEditor.getByText("remote-e2e").click();
  await projectEditor.getByRole("button", { name: "Save changes" }).click();
  await expect(projectEditor).toBeHidden();

  await page.getByRole("tab", { name: /Buckets/ }).click();
  await page.locator(".manage-bucket").first().locator(".catalog-name").click();
  await bucketEditor.getByLabel("Default Worker").selectOption({ label: "remote-e2e" });
  await bucketEditor.getByRole("button", { name: "Save changes" }).click();
  await expect(bucketEditor).toBeHidden();

  await isolatedDaemon.restart();
  await page.reload();
  await logIn(page);
  const pop = page.locator(".spawn-pop");

  // Worker spawn from the bucket ＋, submitted with Enter: the untouched
  // directory stays the project default. Supervisor is what the popover
  // opens on, so this leg picks the worker role explicitly.
  await openSpawn(page, projectName);
  await pop.locator('[data-chip="role"]').click();
  await page.getByRole("menuitemradio", { name: "worker", exact: true }).click();
  await expect(pop).toContainText("worker in");
  await pop.getByRole("button", { name: /permissions, profile, directory/ }).click();
  await expect(pop.locator(".dirpicker input")).toHaveValue(projectPath);
  const title = pop.getByLabel("title (optional)");
  await title.fill("remote default cwd");
  // The restarted remote worker may still be re-registering; Enter carries
  // no actionability wait, so wait for the spawn button to enable first.
  await expect(pop.getByRole("button", { name: "spawn", exact: true })).toBeEnabled();
  await title.press("Enter");
  await expect.poll(() => isolatedDaemon.session("remote default cwd")).toMatchObject({
    cwd: projectPath,
    role: "worker",
    supervisorApi: 0,
  });
  await expect.poll(() => isolatedDaemon.session("remote default cwd")?.workerId ?? 0)
    .toBeGreaterThan(0);

  // Supervisor spawn from the bucket ＋ with an explicit directory override.
  await page.locator('.sb-bucket-row [title^="new session in "]').first().click();
  await expect(pop).toContainText("supervisor in");
  await pop.locator('[data-chip="project"]').click();
  await page.getByRole("menuitemradio", { name: projectName, exact: true }).click();
  await pop.getByRole("button", { name: /permissions, profile, directory/ }).click();
  await pop.locator(".dirpicker input").fill(isolatedDaemon.explicitCwd);
  await pop.getByLabel("title (optional)").fill("remote supervisor override");
  await pop.getByRole("button", { name: "spawn", exact: true }).click();
  await expect.poll(() => isolatedDaemon.session("remote supervisor override")).toMatchObject({
    cwd: isolatedDaemon.explicitCwd,
    role: "supervisor",
    supervisorApi: 1,
  });
  await expect.poll(() => isolatedDaemon.session("remote supervisor override")?.workerId ?? 0)
    .toBeGreaterThan(0);

  // The successful supervisor spawn remembered its home project, so the
  // next bucket ＋ opens with that project as the quiet default.
  await page.locator('.sb-bucket-row [title^="new session in "]').first().click();
  await expect(pop.locator('[data-chip="project"]')).toHaveText(projectName);
  await expect(pop.locator('[data-chip="project"]')).toHaveClass(/is-default/);
  await pop.getByRole("button", { name: "dismiss" }).click();
  await expect(pop).toBeHidden();

  await isolatedDaemon.restart();
  await page.reload();
  await logIn(page, { minimumSessions: 4 });
  await openSpawn(page, projectName);
  await pop.getByRole("button", { name: /permissions, profile, directory/ }).click();
  await expect(pop.locator(".dirpicker input")).toHaveValue(projectPath);
  await pop.getByLabel("title (optional)").fill("remote after override");
  await pop.getByRole("button", { name: "spawn", exact: true }).click();
  await expect.poll(() => isolatedDaemon.session("remote after override")).toMatchObject({
    cwd: projectPath,
  });
});

test("the bucket (+) picks the role, folds the rest, and honors Esc and ✕", async ({
  page,
  isolatedDaemon: _isolatedDaemon,
}) => {
  await logIn(page);
  const spawnButton = page.locator('.sb-bucket-row [title^="new session in "]').first();
  await spawnButton.click();

  const pop = page.locator(".spawn-pop");
  await expect(pop).toContainText("supervisor in");
  await expect(spawnButton).toHaveAttribute("aria-expanded", "true");
  const projectChip = pop.locator('[data-chip="project"]');
  await expect(projectChip).toHaveText(/.+/);
  await expect(projectChip).toHaveClass(/is-default/);

  // A supervisor's APIs are fixed, so it has no items-API checkbox; a worker
  // keeps one in the fold, and switching the role brings it back.
  await pop.getByRole("button", { name: /permissions, profile, directory/ }).click();
  await expect(pop.getByText("permission mode")).toBeVisible();
  await expect(pop.getByText("starting prompt (optional)")).toBeVisible();
  await expect(pop.locator(".spawn-items-api")).toHaveCount(0);
  await pop.locator('[data-chip="role"]').click();
  await page.getByRole("menuitemradio", { name: "worker", exact: true }).click();
  await expect(pop.locator(".spawn-items-api")).toHaveCount(1);

  // Esc closes an open chip menu first, then the popover.
  await projectChip.click();
  await expect(pop.getByRole("menu")).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(pop.getByRole("menu")).toBeHidden();
  await expect(pop).toBeVisible();
  await page.keyboard.press("Escape");
  await expect(pop).toBeHidden();
  await expect(spawnButton).toHaveAttribute("aria-expanded", "false");

  // The bucket ＋ opens on the Supervisor role, which carries no items-API
  // checkbox of its own.
  await openSpawn(page);
  await expect(pop.locator('[data-chip="role"]')).toHaveText("supervisor");
  await expect(pop.locator('[data-chip="project"]')).toHaveCount(1);
  await pop.getByRole("button", { name: /permissions, profile, directory/ }).click();
  await expect(pop.locator(".spawn-items-api")).toHaveCount(0);
  await pop.getByRole("button", { name: "dismiss" }).click();
  await expect(pop).toBeHidden();
});

test("spawn-from-item still opens the full spawn dialog", async ({
  page,
  isolatedDaemon: _isolatedDaemon,
}) => {
  await logIn(page);
  await page.getByRole("button", { name: "open board" }).first().click();
  await page.getByRole("button", { name: "new item" }).click();
  const capture = page.getByRole("dialog", { name: "capture work" });
  await capture.getByLabel("title").fill("Popover leaves this flow alone");
  await capture.getByLabel("description").fill("Items carry a real prompt.");
  await capture.getByRole("button", { name: "create item" }).click();
  await expect(capture).toBeHidden();

  await page.locator(".workbench-row").filter({ hasText: "Popover leaves this flow alone" }).click();
  await page.getByRole("button", { name: "spawn session" }).click();
  const modal = page.locator(".modal");
  await expect(modal).toBeVisible();
  await expect(modal.getByLabel("title (optional)")).toHaveValue("Popover leaves this flow alone");
  await expect(modal.getByLabel("prompt (optional)")).not.toHaveValue("");
  await modal.getByRole("button", { name: "cancel" }).click();
  await expect(modal).toBeHidden();
});

/**
 * The chips sit inline in a sentence that reflows, so no fixed edge suits
 * every one of them: anchoring left sends the last chip's menu off the
 * right, anchoring right sends the first chip's menu off the left. Both
 * have shipped. The menu is measured and pulled back instead, so this
 * checks every chip at several widths rather than one chip at one width.
 */
test("no chip menu escapes the window, at any width", async ({
  page,
  isolatedDaemon: _isolatedDaemon,
}) => {
  await logIn(page);

  for (const width of [1400, 1100, 900, 760]) {
    await page.setViewportSize({ width, height: 900 });
    const pop = await openSpawn(page);

    for (const chip of ["role", "project", "host", "agent"]) {
      const chipButton = pop.locator(`[data-chip="${chip}"]`);
      if ((await chipButton.count()) === 0) continue;
      await chipButton.click();
      const menu = pop.locator(".spawn-chip-menu");
      await expect(menu).toBeVisible();

      const box = await menu.evaluate((element) => {
        const rect = element.getBoundingClientRect();
        return { left: rect.left, right: rect.right, width: rect.width };
      });

      expect(box.left, `${chip} menu at ${width}px is off the left`).toBeGreaterThanOrEqual(0);
      expect(box.right, `${chip} menu at ${width}px is off the right`).toBeLessThanOrEqual(width);
      // A menu clamped into a sliver would satisfy the bounds above while
      // being useless, so it must keep its full width.
      expect(box.width, `${chip} menu at ${width}px was squashed`).toBeGreaterThan(200);

      await chipButton.click();
    }
    await page.keyboard.press("Escape");
  }
});

/**
 * The reported bug: a project whose path is unusable made the Host look
 * like it had dropped off the network, sending people to debug
 * connectivity instead of changing one config field. The local Host is
 * always up here, so anything reading as offline is that bug.
 */
test("a project path that does not exist is named, not reported as an outage", async ({
  page,
  isolatedDaemon,
}) => {
  isolatedDaemon.setProjectPath("/srv/this-path-was-never-created");
  await isolatedDaemon.restart();
  await logIn(page);

  const pop = await openSpawn(page);
  const warning = pop.locator(".field-warn").first();
  await expect(warning).toContainText("/srv/this-path-was-never-created");
  await expect(warning).toContainText("does not exist");
  await expect(warning).not.toContainText("offline");
  await expect(pop.getByRole("button", { name: "spawn" })).toBeDisabled();
});
