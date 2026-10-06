import { expect, test } from "./fixtures";
import { accessToken, logIn, openSettings } from "./support";

test("Devices lists and revokes enrolled devices", async ({ page }) => {
  await logIn(page);
  await openSettings(page, "Devices");
  await expect(page).toHaveURL(/#\/settings\/mobile$/);
  await expect(page.getByText("No devices enrolled")).toBeVisible();

  // Enroll a device through the enrollment token API, then return to the
  // section to observe the device list and audit fields. The page's own fetch
  // carries no credential of its own, so the spec hands it the access token
  // the dashboard would be holding.
  const deviceId = await page.evaluate(async (bearer: string) => {
    const authed = { "Content-Type": "application/json", Authorization: `Bearer ${bearer}` };
    const minted = (await fetch("/api/mobile/devices/enroll-token", {
      method: "POST",
      headers: authed,
      body: "{}",
    }).then((res) => res.json())) as { token: string };
    const enrolled = (await fetch("/api/mobile/devices/enroll", {
      method: "POST",
      headers: authed,
      body: JSON.stringify({
        deviceId: "e2e-app-1",
        name: "e2e phone",
        platform: "ios",
        enrollToken: minted.token,
      }),
    }).then((res) => res.json())) as { device: { id: string } };
    return enrolled.device.id;
  }, await accessToken(page));
  expect(deviceId).toMatch(/^\d+$/);

  await page.getByRole("link", { name: "workers" }).click();
  await page.getByRole("link", { name: "Devices" }).click();

  await expect(page.getByRole("columnheader")).toHaveText(["Device", "Device ID", "Enrolled", "Last seen", "Actions"]);
  const row = page.locator(".device-row", { hasText: "e2e phone" });
  await expect(row).toBeVisible();
  await expect(row.locator(".device-platform")).toHaveText("iOS");
  await expect(row.locator(".device-install-id")).toHaveText("e2e-app-1");
  // Enrolled is a date and a device that never connected says so, each in
  // its own column rather than run together in one audit line.
  await expect(row.getByRole("cell").nth(2)).toHaveText(/\d{4}/);
  await expect(row.getByRole("cell").nth(3)).toHaveText("never");

  await row.getByRole("button", { name: "Revoke" }).click();
  await expect(row).toHaveCount(0);
  await expect(page.getByText("No devices enrolled")).toBeVisible();
});

test("Devices keeps one row when an installation enrolls again", async ({ page }) => {
  await logIn(page);
  await openSettings(page, "Devices");

  const enroll = async (name: string) =>
    await page.evaluate(
      async ([deviceName, bearer]: [string, string]) => {
        const authed = { "Content-Type": "application/json", Authorization: `Bearer ${bearer}` };
        const minted = (await fetch("/api/mobile/devices/enroll-token", {
          method: "POST",
          headers: authed,
          body: "{}",
        }).then((res) => res.json())) as { token: string };
        const enrolled = (await fetch("/api/mobile/devices/enroll", {
          method: "POST",
          headers: authed,
          body: JSON.stringify({
            deviceId: "e2e-app-same",
            name: deviceName,
            platform: "ios",
            enrollToken: minted.token,
          }),
        }).then((res) => res.json())) as { device: { id: string } };
        return enrolled.device.id;
      },
      [name, await accessToken(page)] as [string, string],
    );

  const first = await enroll("first login");
  // The app keeps its device id across a log out, so logging back in is
  // the same installation arriving again.
  const second = await enroll("second login");
  expect(second).toBe(first);

  await page.getByRole("link", { name: "workers" }).click();
  await page.getByRole("link", { name: "Devices" }).click();
  await expect(page.locator(".device-row")).toHaveCount(1);
  await expect(page.locator(".device-row .device-name")).toHaveText("second login");
  await expect(page.locator(".device-row .device-platform")).toHaveText("iOS");
});
