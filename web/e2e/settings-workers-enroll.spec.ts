import { expect, test } from "./fixtures";
import { apiHeaders, logIn, openSettings } from "./support";

test("a Remote worker gets a one-use command at an editable controller URL, in a dialog that keeps it", async ({ page, context, isolatedDaemon }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await logIn(page);
  await openSettings(page, "Workers");
  await expect(page).toHaveURL(/#\/settings\/workers$/);

  const opener = page.getByRole("button", { name: "Add worker" });
  await opener.click();
  const dialog = page.getByRole("dialog", { name: "Add a worker" });
  // Location and the OS are two-way segmented controls, what holds the
  // worker is the one dropdown, and a local worker is never asked which
  // side connects.
  await expect(dialog.getByRole("radio")).toHaveCount(0);
  const location = dialog.getByRole("group", { name: "Location" });
  await expect(location.getByRole("button")).toHaveText(["Local", "Remote"]);
  await expect(location.getByRole("button", { name: "Local" })).toHaveAttribute("aria-pressed", "true");
  await expect(dialog.getByRole("group", { name: "Worker's operating system" }).getByRole("button"))
    .toHaveText(["Linux", "macOS"]);
  await expect(dialog.getByRole("combobox")).toHaveCount(1);
  const holder = dialog.getByLabel("Where it runs");
  await expect(holder.getByRole("option")).toHaveText([
    "Directly on the machine",
    "Docker container",
    "Podman container",
    /^Incus container/,
    "Lima VM",
  ]);
  // A local worker uses the controller's own address or its guest name, so
  // nothing asks for a URL.
  await expect(dialog.getByLabel("Controller URL")).toHaveCount(0);

  // A Lima VM is a guest of the controller's machine, so Remote drops it,
  // and Remote is where the direction is asked, dialing out by default.
  await location.getByRole("button", { name: "Remote" }).click();
  await expect(dialog.getByRole("radio")).toHaveCount(2);
  await expect(dialog.getByRole("radio", { name: /Worker → Controller/ })).toBeChecked();
  await expect(holder.getByRole("option", { name: "Lima VM" })).toHaveCount(0);
  const urlField = dialog.getByLabel("Controller URL");
  // Workers reach the worker plane, which is its own listener on its own port,
  // so the suggestion is never the address this browser is talking to.
  const browser = new URL(process.env.PM_E2E_BASE_URL!);
  await expect(urlField).toHaveValue(new RegExp(`^wss://${browser.hostname}:\\d+$`));

  // Before a command exists there is nothing to lose, so Escape closes the
  // dialog and hands focus back to the button that opened it.
  await page.keyboard.press("Escape");
  await expect(dialog).toHaveCount(0);
  await expect(opener).toBeFocused();
  await opener.click();
  await location.getByRole("button", { name: "Remote" }).click();

  // A container on another machine still launches through --sandbox, and
  // dials the controller's own address rather than a guest name that only
  // resolves on the controller's machine.
  await holder.selectOption({ label: "Docker container" });
  await dialog.getByLabel("Name", { exact: true }).fill("garage-box");
  await dialog.getByRole("button", { name: "Generate command" }).click();
  const command = dialog.locator(".enroll-result .ui-cmd code");
  await expect(command).toHaveText(
    new RegExp(`^pm worker --sandbox --runtime docker --name garage-box --controller wss://${browser.hostname}:\\d+ --token \\S+$`),
  );
  await expect(dialog.getByText(/Shown once\. Works for a single enrollment and expires/)).toBeVisible();
  await expect(dialog.getByText("Nothing installed on that machine yet?")).toBeVisible();

  // The token is shown once, so neither Escape nor a click outside the
  // dialog discards it. Both say why instead.
  await page.keyboard.press("Escape");
  await expect(dialog.getByRole("status")).toHaveText(/shown only once\. Copy it, then choose Done\./);
  await page.mouse.click(4, 4);
  await expect(command).toBeVisible();

  await holder.selectOption({ label: "Directly on the machine" });
  await expect(command).toContainText("pm worker --name garage-box --controller");
  await expect(dialog.getByText("No pm on that machine yet?")).toBeVisible();
  await expect(dialog.locator(".enroll-install .ui-cmd code")).toHaveCount(1);

  // Correcting the URL after minting rewrites the displayed command, and a
  // scheme the worker plane does not speak is corrected rather than pasted
  // through: `pm worker` refuses anything but wss.
  await urlField.fill("http://100.64.0.7:7677");
  await expect(command).toContainText(
    "pm worker --name garage-box --controller wss://100.64.0.7:7677 --token",
  );

  await dialog.locator(".enroll-result").getByRole("button", { name: "Copy", exact: true }).click();
  await expect(dialog.getByRole("button", { name: "Copied" })).toBeVisible();
  const copied = await page.evaluate(() => navigator.clipboard.readText());
  const match = /^pm worker --name garage-box --controller wss:\/\/100\.64\.0\.7:7677 --token (\S+)$/.exec(copied);
  expect(match).not.toBeNull();

  // Done is the one way out once a command is on screen.
  await dialog.getByRole("button", { name: "Done" }).click();
  await expect(dialog).toHaveCount(0);

  // The minted token enrolls a real worker, and the list reflects it live.
  await isolatedDaemon.startRemoteWorker(match![1]);
  const row = page.locator(".worker-row", { hasText: "garage-box" });
  await expect(row).toBeVisible();
  await expect(row.locator(".worker-status")).toHaveText("Online");

  // Closing dropped the one-use command: opening the dialog again starts clean.
  await opener.click();
  await expect(dialog.getByText("The command appears here")).toBeVisible();
  await expect(page.locator(".ui-cmd")).toHaveCount(0);
});

test("Studio keeps the multi-line worker form blocks off its pill radius, and the dialog fits a phone", async ({ page }) => {
  await logIn(page);
  const settings = `${process.env.PM_E2E_BASE_URL!}/api/user/settings/ui-theme`;
  const headers = { ...(await apiHeaders(page)), "content-type": "application/json" };
  await page.request.put(settings, { headers, data: JSON.stringify("studio") });
  await expect(page.locator("html")).toHaveAttribute("data-theme", "studio");
  await openSettings(page, "Workers");
  await page.getByRole("button", { name: "Add worker" }).click();
  const dialog = page.getByRole("dialog", { name: "Add a worker" });

  // Buttons are pills in Studio. The direction choices, asked of a remote
  // worker, run to several lines, and so does the dialog itself, so both
  // take a block radius instead.
  await expect(dialog.getByRole("button", { name: "Generate command" })).toHaveCSS("border-top-left-radius", "999px");
  await dialog.getByRole("group", { name: "Location" }).getByRole("button", { name: "Remote" }).click();
  await expect(dialog.locator(".ui-choice label").first()).toHaveCSS("border-top-left-radius", "12px");
  await expect(dialog).toHaveCSS("border-top-left-radius", "14px");

  // At phone width the command column moves under the form and the dialog
  // stays inside the screen.
  await page.setViewportSize({ width: 390, height: 844 });
  const columns = await dialog.locator(".ui-cols > div").evaluateAll((elements) =>
    elements.map((element) => element.getBoundingClientRect()));
  expect(columns[1].top).toBeGreaterThanOrEqual(columns[0].bottom);
  const box = await dialog.evaluate((element) => {
    const rect = element.getBoundingClientRect();
    return {
      left: rect.left,
      right: rect.right,
      bottom: rect.bottom,
      overflow: element.scrollWidth - element.clientWidth,
      width: document.documentElement.clientWidth,
      height: window.innerHeight,
    };
  });
  expect(box.left).toBeGreaterThanOrEqual(0);
  expect(box.right).toBeLessThanOrEqual(box.width);
  expect(box.bottom).toBeLessThanOrEqual(box.height);
  expect(box.overflow).toBeLessThanOrEqual(0);
  await expect(dialog.getByRole("button", { name: "Cancel" })).toBeInViewport();
  await page.request.delete(settings, { headers });
});

test("a daemon with a public URL offers it as the controller address, at the worker plane's port", async ({
  page,
  isolatedDaemon,
}) => {
  await isolatedDaemon.restart({ publicUrl: "https://pm.example.test" });
  await logIn(page);
  await openSettings(page, "Workers");
  await page.getByRole("button", { name: "Add worker" }).click();
  const dialog = page.getByRole("dialog", { name: "Add a worker" });
  await dialog.getByRole("group", { name: "Location" }).getByRole("button", { name: "Remote" }).click();
  // The browser reached the daemon at 127.0.0.1, which no other machine can,
  // so the operator's configured name wins over the address in the location bar.
  await expect(dialog.getByLabel("Controller URL")).toHaveValue(/^wss:\/\/pm\.example\.test:\d+$/);
  await dialog.getByLabel("Name", { exact: true }).fill("named-box");
  await dialog.getByRole("button", { name: "Generate command" }).click();
  await expect(dialog.locator(".enroll-result .ui-cmd code"))
    .toHaveText(/^pm worker --name named-box --controller wss:\/\/pm\.example\.test:\d+ --token \S+$/);
});
