import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

// The seeded board holds 75 low-priority items ranked newest first, so the
// first page ends at "Volume item 25" and the second carries the rest.
const FIRST_ROW = "Volume item 74";
const LAST_ROW = "Volume item 00";
const SECOND_PAGE_ROW = "Volume item 05";

async function openBoard(page: Page): Promise<void> {
  await logIn(page);
  await page.locator(".sb-board-link").click();
  await expect(page.locator(".workbench")).toBeVisible();
}

function recordItemQueryOffsets(page: Page): string[] {
  const offsets: string[] = [];
  page.on("request", (request) => {
    const [path, search] = request.url().split("?");
    if (!/\/api\/buckets\/\d+\/items$/.test(path) || search === undefined) return;
    offsets.push(new URLSearchParams(search).get("offset") ?? "");
  });
  return offsets;
}

async function loadBothPages(page: Page): Promise<void> {
  const rows = page.locator(".workbench-row");
  await expect(rows).toHaveCount(50);
  await page.getByRole("button", { name: "load 50 more" }).click();
  await expect(rows).toHaveCount(75);
}

test("editing an item keeps every loaded page and the index scroll position", async ({ page }) => {
  const offsets = recordItemQueryOffsets(page);
  await openBoard(page);
  await loadBothPages(page);

  const rows = page.locator(".workbench-row");
  // The top-ranked item stays top-ranked once it is urgent, so the refresh
  // reorders nothing and the recorded scroll offset must survive verbatim.
  await rows.filter({ hasText: FIRST_ROW }).click();
  const results = page.locator(".workbench-index-results");
  const scrolled = await results.evaluate((element) => {
    element.scrollTop = element.scrollHeight;
    return element.scrollTop;
  });
  expect(scrolled).toBeGreaterThan(0);

  offsets.length = 0;
  await page.locator(".workflow-priority").getByRole("button", { name: "urgent" }).click();
  await expect(page.locator(".workbench-toast")).toContainText("Priority set to urgent");
  await expect.poll(() => offsets.length, { message: "the refresh must cover both loaded pages" }).toBe(2);
  expect(offsets).toEqual(["0", "50"]);

  await expect(rows).toHaveCount(75);
  await expect(rows.first()).toContainText(FIRST_ROW);
  await expect(rows.filter({ hasText: LAST_ROW })).toHaveCount(1);
  expect(await results.evaluate((element) => element.scrollTop)).toBe(scrolled);
});

test("an edit that leaves the active filter drops the row without collapsing the window", async ({ page }) => {
  const offsets = recordItemQueryOffsets(page);
  await openBoard(page);
  const filtered = page.waitForResponse((response) =>
    response.url().includes("priority=low") && response.url().includes("offset=0"));
  await page.getByLabel("filter by priority").selectOption("low");
  await filtered;
  await loadBothPages(page);

  const rows = page.locator(".workbench-row");
  await rows.filter({ hasText: SECOND_PAGE_ROW }).click();
  offsets.length = 0;
  await page.locator(".workflow-priority").getByRole("button", { name: "urgent" }).click();
  await expect(page.locator(".workbench-toast")).toContainText("Priority set to urgent");

  await expect(rows).toHaveCount(74);
  await expect(rows.filter({ hasText: SECOND_PAGE_ROW })).toHaveCount(0);
  await expect(rows.filter({ hasText: LAST_ROW })).toHaveCount(1);
  expect(offsets).toEqual(["0", "50"]);
});
