import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { expect, test } from "./fixtures";
import { logIn } from "./support";

const SESSION = "browser-e2e";
const COLLAPSED_HEIGHT = 24;
const DRAG_PX = 120;

async function openSession(page: import("@playwright/test").Page) {
  await page
    .locator(".sb-session")
    .filter({ has: page.locator(".sb-session-title").getByText(SESSION, { exact: true }) })
    .click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);
}

function layoutOf(text: string): { rev: number; layout: unknown; unplaced: string[]; panes: Array<{ id: string; state: string }> } {
  return JSON.parse(text);
}

test("an agent's panes sit beside it and the user resizes, collapses, hides, restores and stops them", async ({ page, isolatedDaemon }) => {
  await logIn(page);
  await openSession(page);
  const cwd = isolatedDaemon.session(SESSION)!.cwd;
  await mkdir(join(cwd, "status"), { recursive: true });
  await writeFile(join(cwd, "status", "index.html"), "<!doctype html><h1>Status page</h1>");

  await isolatedDaemon.callAgentTool(SESSION, "open_page", { id: "status", title: "Build status", dir: "status" });
  const statusPane = page.locator(".ap-pane", { has: page.locator(".ap-title", { hasText: "Build status" }) });
  await expect(statusPane).toBeVisible();
  const frame = statusPane.locator("iframe");
  await expect(frame).toHaveAttribute("sandbox", "allow-scripts allow-forms");
  await expect(page.frameLocator(".ap-pane iframe").getByRole("heading", { name: "Status page" })).toBeVisible();

  isolatedDaemon.setSessionPermission(SESSION, "bypass");
  await isolatedDaemon.callAgentTool(SESSION, "open_terminal", { id: "job", title: "Echo job", command: "echo pane-ready; sleep 60" });
  const jobPane = page.locator(".ap-pane", { has: page.locator(".ap-title", { hasText: "Echo job" }) });
  await expect(jobPane.locator(".ap-status")).toHaveText("running");
  await expect.poll(async () => JSON.parse(await isolatedDaemon.callAgentTool(SESSION, "read_pane", { id: "job" })).text).toContain("pane-ready");
  await expect(page.locator(".terminal-tabs .terminal-tab", { hasText: "Echo job" })).toHaveCount(0);

  await jobPane.getByRole("button", { name: "Collapse pane" }).click();
  await expect(jobPane).toHaveClass(/is-collapsed/);
  await expect.poll(async () => (await jobPane.boundingBox())?.height).toBe(COLLAPSED_HEIGHT);
  await jobPane.getByRole("button", { name: "Expand pane" }).click();
  await expect(jobPane).not.toHaveClass(/is-collapsed/);

  const before = layoutOf(await isolatedDaemon.callAgentTool(SESSION, "get_layout", {}));
  const gutter = page.locator(".ap-gutter-row").first();
  const box = (await gutter.boundingBox())!;
  // A quarter of the way down, clear of where the stacked panes' divider meets this one.
  const grabY = box.y + box.height / 4;
  await page.mouse.move(box.x + box.width / 2, grabY);
  await page.mouse.down();
  await page.mouse.move(box.x - DRAG_PX / 2, grabY, { steps: 4 });
  await page.mouse.move(box.x - DRAG_PX, grabY, { steps: 4 });
  await page.mouse.up();
  await expect.poll(async () => layoutOf(await isolatedDaemon.callAgentTool(SESSION, "get_layout", {})).rev).toBeGreaterThan(before.rev);
  await expect.poll(async () => (await gutter.boundingBox())!.x).toBeLessThan(box.x - DRAG_PX / 2);

  await statusPane.getByRole("button", { name: "Hide" }).click();
  await expect(statusPane).toHaveCount(0);
  const chip = page.getByRole("button", { name: "↺ Build status" });
  await expect(chip).toBeVisible();
  expect(layoutOf(await isolatedDaemon.callAgentTool(SESSION, "get_layout", {})).unplaced).toEqual(["status"]);
  await chip.click();
  await expect(statusPane).toBeVisible();
  await expect(chip).toHaveCount(0);

  await jobPane.getByRole("button", { name: "Stop" }).click();
  const dialog = page.getByRole("dialog", { name: "stop this program" });
  await expect(dialog).toBeVisible();
  const dialogBox = (await dialog.boundingBox())!;
  const viewport = page.viewportSize()!;
  expect(Math.abs(dialogBox.x + dialogBox.width / 2 - viewport.width / 2)).toBeLessThan(viewport.width / 10);
  await dialog.getByRole("button", { name: "stop" }).click();
  await expect(jobPane).toHaveCount(0);
  const after = layoutOf(await isolatedDaemon.callAgentTool(SESSION, "get_layout", {}));
  expect(after.panes.map((pane) => pane.id)).toEqual(["status"]);

  await isolatedDaemon.callAgentTool(SESSION, "open_terminal", { id: "short", title: "Short job", command: "echo finished-output; exit 4" });
  const shortPane = page.locator(".ap-pane", { has: page.locator(".ap-title", { hasText: "Short job" }) });
  await expect(shortPane.locator(".ap-status")).toHaveText("exit 4");
  await expect(shortPane.locator(".ap-output")).toContainText("finished-output");
  await expect(shortPane.locator(".ap-output")).toContainText("[exited with status 4]");
});

test("terminal panes need bypass permissions while page panes do not", async ({ page, isolatedDaemon }) => {
  await logIn(page);
  isolatedDaemon.setSessionPermission(SESSION, "default");
  await expect(isolatedDaemon.callAgentTool(SESSION, "open_terminal", { id: "job", title: "Job", command: "true" }))
    .rejects.toThrow(/needs bypass permissions/);
  await expect(isolatedDaemon.callAgentTool(SESSION, "send_to_pane", { id: "job", text: "x" }))
    .rejects.toThrow(/needs bypass permissions/);
});
