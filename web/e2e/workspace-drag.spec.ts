import type { JSHandle } from "@playwright/test";
import { expect, test, type Locator, type Page } from "./fixtures";
import { logIn } from "./support";

const LAYOUT_SAVE_URL = /\/api\/workspaces\/\d+$/;

async function createSplitWorkspace(page: Page): Promise<string[]> {
  await logIn(page);
  await page.getByTitle("new workspace").click();
  await page.getByRole("button", { name: "create workspace" }).click();
  await expect(page.locator(".workspace-pane")).toHaveCount(1);
  await page.getByTitle("split right").first().click();
  await expect(page.locator(".workspace-pane")).toHaveCount(2);

  const values = await page.getByLabel("terminal shown in pane").first().locator("option").evaluateAll((options) =>
    options
      .map((option) => (option as HTMLOptionElement).value)
      .filter((value) => value !== ""));
  expect(values.length).toBeGreaterThanOrEqual(2);
  return values;
}

async function openWorkspaceWithTwoPanes(page: Page): Promise<string[]> {
  const values = await createSplitWorkspace(page);
  const selectors = page.getByLabel("terminal shown in pane");
  await selectors.nth(0).selectOption(values[0]);
  await selectors.nth(1).selectOption(values[1]);
  await expect(selectors.nth(0)).toHaveValue(values[0]);
  await expect(selectors.nth(1)).toHaveValue(values[1]);
  return values;
}

async function startPaneDrag(page: Page, header: Locator): Promise<JSHandle<DataTransfer>> {
  const dataTransfer = await page.evaluateHandle(() => new DataTransfer());
  await header.dispatchEvent("dragstart", { dataTransfer });
  return dataTransfer;
}

async function dragOverAt(target: Locator, dataTransfer: JSHandle<DataTransfer>, xRatio: number, yRatio: number): Promise<void> {
  const box = await target.boundingBox();
  if (!box) throw new Error("target pane is not visible");
  await target.dispatchEvent("dragover", {
    dataTransfer,
    clientX: box.x + box.width * xRatio,
    clientY: box.y + box.height * yRatio,
  });
}

async function dropAt(target: Locator, dataTransfer: JSHandle<DataTransfer>, xRatio: number, yRatio: number): Promise<void> {
  const box = await target.boundingBox();
  if (!box) throw new Error("target pane is not visible");
  await target.dispatchEvent("drop", {
    dataTransfer,
    clientX: box.x + box.width * xRatio,
    clientY: box.y + box.height * yRatio,
  });
}

test.describe.serial("workspace pane drag", () => {
  test("shows a drag ghost chip and a single morphing drop region", async ({ page }) => {
    await openWorkspaceWithTwoPanes(page);
    const source = page.locator(".workspace-pane-head").first();
    const target = page.locator(".workspace-pane").nth(1);

    await expect(page.locator(".pane-drag-ghost")).toHaveCount(0);
    const dataTransfer = await startPaneDrag(page, source);
    await expect(page.locator(".pane-drag-ghost")).toHaveCount(1);
    await expect(page.locator(".pane-drag-ghost-label")).not.toBeEmpty();
    await expect(page.locator(".workspace-pane").first()).toHaveClass(/is-drag-source/);

    await dragOverAt(target, dataTransfer, 0.1, 0.5);
    const region = target.locator(".pane-drop-region");
    await expect(region).toHaveCount(1);
    await expect(region).toHaveClass(/drop-left/);
    await expect(region).toHaveAttribute("style", /left: 0%/);
    await expect(region).toHaveAttribute("style", /width: 50%/);
    await expect(target).toHaveClass(/is-drop-target/);

    await dragOverAt(target, dataTransfer, 0.4, 0.5);
    await expect(region).toHaveCount(1);
    await expect(region).toHaveClass(/drop-swap/);
    await expect(region).toHaveAttribute("style", /width: 100%/);
    await expect(region).toHaveAttribute("style", /height: 100%/);

    await dragOverAt(target, dataTransfer, 0.5, 0.9);
    await expect(region).toHaveClass(/drop-below/);
    await expect(region).toHaveAttribute("style", /top: 50%/);
    await expect(region).toHaveAttribute("style", /height: 50%/);

    await source.dispatchEvent("dragend");
    await expect(page.locator(".pane-drag-ghost")).toHaveCount(0);
    await expect(page.locator(".workspace-pane.is-drag-source")).toHaveCount(0);
  });

  test("dropping in the pane center swaps the two terminals", async ({ page }) => {
    const values = await openWorkspaceWithTwoPanes(page);
    const source = page.locator(".workspace-pane-head").first();
    const target = page.locator(".workspace-pane").nth(1);

    const dataTransfer = await startPaneDrag(page, source);
    await dragOverAt(target, dataTransfer, 0.4, 0.5);
    await dropAt(target, dataTransfer, 0.4, 0.5);
    await source.dispatchEvent("dragend");

    const selectors = page.getByLabel("terminal shown in pane");
    await expect(selectors).toHaveCount(2);
    await expect(selectors.nth(0)).toHaveValue(values[1]);
    await expect(selectors.nth(1)).toHaveValue(values[0]);
    await expect(page.locator(".pane-drop-region")).toHaveCount(0);
  });

  test("dropping on the bottom edge moves the pane below the target", async ({ page }) => {
    const values = await openWorkspaceWithTwoPanes(page);
    await expect(page.locator(".workspace-split.split-columns")).toHaveCount(1);
    const source = page.locator(".workspace-pane-head").first();
    const target = page.locator(".workspace-pane").nth(1);

    const dataTransfer = await startPaneDrag(page, source);
    await dragOverAt(target, dataTransfer, 0.5, 0.9);
    await dropAt(target, dataTransfer, 0.5, 0.9);
    await source.dispatchEvent("dragend");

    await expect(page.locator(".workspace-split.split-rows")).toHaveCount(1);
    await expect(page.locator(".workspace-split.split-columns")).toHaveCount(0);
    const selectors = page.getByLabel("terminal shown in pane");
    await expect(selectors).toHaveCount(2);
    await expect(selectors.nth(0)).toHaveValue(values[1]);
    await expect(selectors.nth(1)).toHaveValue(values[0]);
  });

  test("quick pane assignments survive a slow in-flight layout save", async ({ page }) => {
    await page.route(LAYOUT_SAVE_URL, async (route) => {
      if (route.request().method() === "PUT") await new Promise((resolve) => setTimeout(resolve, 700));
      await route.continue();
    });
    const values = await createSplitWorkspace(page);
    const selectors = page.getByLabel("terminal shown in pane");

    const saveInFlight = page.waitForRequest((request) => request.method() === "PUT" && LAYOUT_SAVE_URL.test(request.url()));
    await selectors.nth(0).selectOption(values[0]);
    const staleSave = await saveInFlight;
    await selectors.nth(1).selectOption(values[1]);
    await page.waitForResponse((response) => response.request() === staleSave);

    await expect(selectors.nth(0)).toHaveValue(values[0]);
    await expect(selectors.nth(1)).toHaveValue(values[1]);
  });
});
