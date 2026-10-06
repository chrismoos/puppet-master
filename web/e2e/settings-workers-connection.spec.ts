import { expect, test, type Page } from "./fixtures";
import { logIn, openSettings } from "./support";

async function openAddWorker(page: Page) {
  await page.getByRole("button", { name: "Add worker" }).click();
  const form = page.getByRole("dialog", { name: "Add a worker" });
  await expect(form).toBeVisible();
  return form;
}

async function openReenroll(page: Page, name: string) {
  await page.locator(".worker-row", { hasText: name }).getByRole("button", { name: "Re-enroll" }).click();
  const dialog = page.getByRole("dialog", { name: `Re-enroll ${name}` });
  await expect(dialog).toBeVisible();
  return dialog;
}

test("Workers asks which end connects and builds a listening command for a dialed worker", async ({
  page,
}) => {
  await logIn(page);
  await openSettings(page, "Workers");
  await expect(page).toHaveURL(/#\/settings\/workers$/);

  // The form opens in a dialog from the page's own action.
  await expect(page.getByRole("dialog")).toHaveCount(0);
  const form = await openAddWorker(page);

  // The controller-dials direction asks for an address and drops the
  // controller URL, which only a worker that dials out ever needs.
  await form.getByRole("group", { name: "Location" }).getByRole("button", { name: "Remote" }).click();
  await form.getByRole("radio", { name: /Controller → Worker/ }).check();
  await expect(form.getByLabel("Where it runs")).toHaveCount(0);
  await expect(form.getByLabel("Controller URL")).toHaveCount(0);
  const address = form.getByLabel("Worker address");
  await expect(address).toBeVisible();

  await address.fill("10.0.0.5");
  await expect(form.getByText("Not a host:port address.")).toBeVisible();
  await form.getByLabel("Name", { exact: true }).fill("garage-box");
  await form.getByRole("button", { name: "Generate command" }).click();
  await expect(form.getByRole("alert")).toContainText("needs an address to dial");
  await expect(page.locator(".ui-cmd")).toHaveCount(0);

  // A real address mints a command that waits on it rather than dialing out.
  await address.fill("10.0.0.5:7677");
  await expect(form.getByText("Not a host:port address.")).toHaveCount(0);
  await form.getByRole("button", { name: "Generate command" }).click();
  const command = form.locator(".enroll-result .ui-cmd code");
  await expect(command).toHaveText(/^pm worker --name garage-box --listen 10\.0\.0\.5:7677 --token \S+$/);
  await expect(command).not.toContainText("--controller");
  await expect(form.getByText("garage-box appears in the list as Pending until the command runs.")).toBeVisible();

  // Adding is durable before the machine enrolls: it appears immediately,
  // says why it is pending, and can mint a replacement token.
  const row = page.locator(".worker-row", { hasText: "garage-box" });
  await expect(row).toBeVisible();
  await expect(row.locator(".worker-status")).toHaveText("Pending");
  await expect(row).toContainText("enroll command not run yet");

  // Asking again under the same name replaces the pending worker's token
  // rather than adding a second worker of that name.
  const first = await command.textContent();
  await form.getByRole("button", { name: "Generate new command" }).click();
  await expect(command).toHaveText(/^pm worker --name garage-box --listen 10\.0\.0\.5:7677 --token \S+$/);
  await expect(command).not.toHaveText(first!);
  await expect(page.locator(".worker-row", { hasText: "garage-box" })).toHaveCount(1);

  await form.getByRole("button", { name: "Done" }).click();
  await expect(form).toHaveCount(0);
  const reenroll = await openReenroll(page, "garage-box");
  await reenroll.getByRole("button", { name: "Generate command" }).click();
  await expect(reenroll.locator(".enroll-result .ui-cmd code")).toHaveText(
    /^pm worker --name garage-box --listen 10\.0\.0\.5:7677 --token \S+$/,
  );
});

test("re-enrolling can move a worker to a new address without re-adding it", async ({ page }) => {
  await logIn(page);
  await openSettings(page, "Workers");

  const form = await openAddWorker(page);
  await form.getByRole("group", { name: "Location" }).getByRole("button", { name: "Remote" }).click();
  await form.getByRole("radio", { name: /Controller → Worker/ }).check();
  await form.getByLabel("Worker address").fill("10.0.0.5:7677");
  await form.getByLabel("Name", { exact: true }).fill("moving-box");
  await form.getByRole("button", { name: "Generate command" }).click();
  await expect(form.locator(".enroll-result .ui-cmd code")).toBeVisible();
  await form.getByRole("button", { name: "Done" }).click();

  // The address a worker is dialed at is not fixed at enrollment: re-enrolling
  // is where it moves, and the command follows the new one.
  const row = page.locator(".worker-row", { hasText: "moving-box" });
  const reenroll = await openReenroll(page, "moving-box");
  const address = reenroll.getByLabel("Address the controller dials");
  await expect(address).toHaveValue("10.0.0.5:7677");
  await address.fill("10.9.9.9:7000");
  await reenroll.getByRole("button", { name: "Generate command" }).click();
  await expect(reenroll.locator(".enroll-result .ui-cmd code")).toHaveText(
    /^pm worker --name moving-box --listen 10\.9\.9\.9:7000 --token \S+$/,
  );
  // The row keeps naming the address the controller can reach today. The
  // move lands when the worker completes the command, so an abandoned
  // re-enrollment leaves a working worker working rather than stranding it.
  await expect(reenroll.getByText(/still reads as 10\.0\.0\.5:7677 until it completes/)).toBeVisible();
  await reenroll.getByRole("button", { name: "Done" }).click();
  await expect(row.locator(".worker-connect-mode code")).toHaveText("10.0.0.5:7677");
});

test("re-enrolling asks where a worker is and what holds it", async ({ page }) => {
  await logIn(page);
  await openSettings(page, "Workers");

  const form = await openAddWorker(page);
  await form.getByLabel("Name", { exact: true }).fill("guest-box");
  await form.getByRole("button", { name: "Generate command" }).click();
  // The worker plane is its own listener and speaks only wss, so the command
  // never names the address this browser is talking to.
  await expect(form.locator(".enroll-result .ui-cmd code"))
    .toHaveText(/^pm worker --name guest-box --controller wss:\/\/\S+ --token \S+$/);
  await form.getByRole("button", { name: "Done" }).click();

  const reenroll = await openReenroll(page, "guest-box");
  // Nothing a worker reports places it on the controller's machine, so
  // re-enrolling starts as remote, where Lima is not offered.
  const location = reenroll.getByRole("group", { name: "Location" });
  await expect(location.getByRole("button", { name: "Remote" })).toHaveAttribute("aria-pressed", "true");
  const holder = reenroll.getByLabel("Where it runs");
  await expect(holder.getByRole("option", { name: "Lima VM" })).toHaveCount(0);
  await location.getByRole("button", { name: "Local" }).click();
  await expect(reenroll.getByLabel("Controller URL")).toHaveCount(0);
  await expect(reenroll.getByRole("radio")).toHaveCount(0);

  // Re-enroll asks for the worker's operating system as adding one does, and
  // offers only what that system can run.
  await reenroll.getByRole("group", { name: "Worker's operating system" }).getByRole("button", { name: "macOS" }).click();
  await expect(holder.getByRole("option", { name: /Incus/ })).toBeDisabled();
  await holder.selectOption({ label: "Lima VM" });
  await reenroll.getByRole("button", { name: "Generate command" }).click();
  const command = reenroll.locator(".enroll-result .ui-cmd code");
  await expect(command).toHaveText(
    /^pm worker --name guest-box --controller wss:\/\/host\.lima\.internal:\d+ --token \S+$/,
  );

  await holder.selectOption({ label: "Docker container" });
  await reenroll.getByRole("button", { name: "Generate new command" }).click();
  await expect(command).toHaveText(
    /^pm worker --sandbox --runtime docker --name guest-box --controller wss:\/\/host\.docker\.internal:\d+ --token \S+$/,
  );
});

test("Workers labels a dialing worker and re-enrolls it without disturbing its id", async ({
  page,
  context,
  isolatedDaemon,
}) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await logIn(page);
  await openSettings(page, "Workers");

  const form = await openAddWorker(page);
  await form.getByLabel("Name", { exact: true }).fill("build-box");
  await form.getByRole("button", { name: "Generate command" }).click();
  const minted = await form.locator(".enroll-result .ui-cmd code").textContent();
  const token = /--token (\S+)$/.exec(minted!)![1];
  await form.getByRole("button", { name: "Done" }).click();

  await isolatedDaemon.startRemoteWorker(token);
  const row = page.locator(".worker-row", { hasText: "build-box" });
  await expect(row).toBeVisible();
  await expect(row.locator(".worker-status")).toHaveText("Online");

  // Status, platform, connection and version each have their own column.
  await expect(page.getByRole("columnheader")).toHaveText(
    ["Worker", "Status", "Platform", "Connection", "Version", "Actions"],
  );
  await expect(row.locator(".worker-version")).toHaveText(/^pm \d/);

  // A worker that dialed the controller says so, and carries no dialed address.
  const mode = row.locator(".worker-connect-mode");
  await expect(mode).toHaveText("dials controller");
  await expect(mode.locator("code")).toHaveCount(0);

  // Re-enrolling rotates the credential in place: same row, same id, and a
  // command that keeps this worker dialing the controller.
  const reenroll = await openReenroll(page, "build-box");
  await reenroll.getByRole("button", { name: "Generate command" }).click();
  const rotated = reenroll.locator(".enroll-result .ui-cmd code");
  await expect(rotated).toHaveText(/^pm worker --name build-box --controller wss:\/\/\S+ --token \S+$/);
  expect(await rotated.textContent()).not.toContain(token);
  await expect(reenroll.getByText(/keeps its id/)).toBeVisible();
  await expect(page.locator(".worker-row", { hasText: "build-box" })).toHaveCount(1);

  await reenroll.locator(".enroll-result").getByRole("button", { name: "Copy", exact: true }).click();
  await expect(reenroll.getByRole("button", { name: "Copied" })).toBeVisible();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe(
    await rotated.textContent(),
  );

  // The rotated token is shown once, so only Done closes the dialog, and
  // closing drops the command from the screen.
  await page.keyboard.press("Escape");
  await expect(reenroll.getByRole("status")).toHaveText(/shown only once/);
  await reenroll.getByRole("button", { name: "Done" }).click();
  await expect(reenroll).toHaveCount(0);
  await expect(row.getByRole("button", { name: "Re-enroll" })).toBeFocused();
  await expect(page.locator(".ui-cmd")).toHaveCount(0);
});
