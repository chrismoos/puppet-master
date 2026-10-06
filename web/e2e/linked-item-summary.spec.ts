import { execFileSync } from "node:child_process";
import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

const LONG_TITLE = "Linked board work title that reveals more text as the session sidebar becomes wider";

function cli(args: string[]): string {
  return execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
}

test("actual session rows show, resize, and internally route linked items", async ({ page }) => {
  const output = cli([
    "items", "add", "--bucket", "1", "--project", "1", "--status", "planned", LONG_TITLE,
  ]);
  const itemId = output.match(/pm:item\/1\/(\d+) created/)?.[1];
  if (!itemId) throw new Error(`could not parse created item id from ${output}`);

  await logIn(page);
  await page.goto(`${process.env.PM_E2E_BASE_URL!}/#/bucket/1/item/${itemId}`);
  await expect(page.getByLabel("item title")).toHaveValue(LONG_TITLE);
  await page.getByRole("button", { name: "spawn session" }).click();
  await page.locator(".modal").getByRole("button", { name: "spawn", exact: true }).click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);
  const sessionId = page.url().match(/#\/session\/(\d+)$/)?.[1];
  if (!sessionId) throw new Error(`spawn did not navigate to a session: ${page.url()}`);
  await expect.poll(() => cli(["items", "show", itemId])).toContain(`sessions ${sessionId}`);

  const row = page.locator(".sb-session-row").filter({ hasText: LONG_TITLE }).last();
  const link = row.locator(".sb-linked-item");
  await expect(link).toHaveAttribute("href", `#/bucket/1/item/${itemId}`);
  await expect(link).toHaveAttribute("title", `#${itemId} · ${LONG_TITLE} — in progress`);
  await expect(link.locator(".sb-linked-item-status")).toHaveCount(0);

  const workspace = page.locator(".workspace");
  const widths: number[] = [];
  for (const sidebarWidth of [220, 620]) {
    await workspace.evaluate((element, width) => {
      (element as HTMLElement).style.setProperty("--sidebar-w", `${width}px`);
    }, sidebarWidth);
    const metrics = await link.locator(".sb-linked-item-title").evaluate((element) => ({
      clientWidth: element.clientWidth,
      scrollWidth: element.scrollWidth,
    }));
    widths.push(metrics.clientWidth);
    expect(await row.evaluate((element) => element.scrollWidth <= element.clientWidth)).toBe(true);
    expect(metrics.scrollWidth > metrics.clientWidth).toBe(sidebarWidth === 220);
  }
  expect(widths[1]).toBeGreaterThan(widths[0]);

  await link.click();
  await expect(page).toHaveURL(new RegExp(`#\\/bucket\\/1\\/item\\/${itemId}$`));
  await row.locator(".sb-session").click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);
  // Opening a session moves focus into its terminal from a later effect, which
  // would otherwise take the keyboard back from the link mid-press.
  await expect.poll(() => page.evaluate(() => document.activeElement?.className ?? ""))
    .toContain("xterm-helper-textarea");
  await link.press("Space");
  await expect(page).toHaveURL(new RegExp(`#\\/bucket\\/1\\/item\\/${itemId}$`));

  cli(["items", "done", itemId]);
  await expect(link.locator(".sb-linked-item-status")).toHaveCount(0);
  await expect(link).toHaveAttribute("title", `#${itemId} · ${LONG_TITLE} — done`);

  cli(["items", "rm", itemId]);
  await expect(row.locator(".sb-linked-item")).toHaveCount(0);
});
