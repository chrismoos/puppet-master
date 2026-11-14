import { expect, test } from "./fixtures";
import { logIn } from "./support";

const WATCHED = "browser-e2e";
const OTHER = "browser-e2e-two";

test.beforeEach(async ({ page }) => {
  await page.addInitScript(() => localStorage.setItem("pm.notify.enabled", "1"));
});

test("an agent's notify_user call shows as a card that opens its session, and cards stack", async ({ page, isolatedDaemon }) => {
  await logIn(page);
  await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title").getByText(WATCHED, { exact: true }) }).click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);
  const watchedUrl = page.url();
  await page.bringToFront();

  await isolatedDaemon.callAgentTool(OTHER, "notify_user", { title: "Deploy finished", body: "staging is green" });
  const stack = page.getByRole("region", { name: "Alerts" });
  const first = stack.locator(".alert-card-message");
  await expect(first).toHaveCount(1);
  await expect(first.locator(".alert-card-title")).toHaveText(OTHER);
  await expect(first.locator(".alert-card-body")).toHaveText("Deploy finished staging is green");

  await expect(isolatedDaemon.callAgentTool(OTHER, "notify_user", { title: "Again" }))
    .rejects.toThrow(/one notification every 10 seconds/);

  const items = JSON.parse(await isolatedDaemon.callAgentTool(WATCHED, "upsert_items", {
    items: [{ external_key: "e2e:notify-link", title: "Fix the login page", status: "planned" }],
  })) as { results: Array<{ id: number }> };
  const listed = JSON.parse(await isolatedDaemon.callAgentTool(WATCHED, "list_items", { search: "Fix the login page" })) as {
    items: Array<{ id: number; ref: string }>;
  };
  const ref = listed.items.find((item) => item.id === items.results[0].id)!.ref;
  await isolatedDaemon.callAgentTool(WATCHED, "notify_user", { title: "Filed it", link: ref });

  const cards = stack.locator(".alert-card");
  await expect(cards).toHaveCount(2);
  await expect(cards.first().locator(".alert-card-title")).toHaveText(WATCHED);
  await expect(cards.nth(1)).toHaveClass(/is-compact/);
  await expect(stack.getByRole("button", { name: "Dismiss all" })).toBeVisible();
  const link = cards.first().locator(".alert-card-link");
  await expect(link.locator(".alert-card-link-title")).toHaveText("Fix the login page");
  await expect(link).toHaveAttribute("href", /^#\/bucket\/\d+\/item\/\d+$/);

  await cards.first().getByRole("button", { name: "Dismiss" }).click();
  await expect(cards).toHaveCount(1);
  await expect(page).toHaveURL(watchedUrl);

  await first.getByRole("button", { name: `Open ${OTHER}` }).click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);
  expect(page.url()).not.toBe(watchedUrl);
  await expect(page.locator(".sb-session.is-selected .sb-session-title")).toHaveText(OTHER);
  await expect(stack).toHaveCount(0);
});
