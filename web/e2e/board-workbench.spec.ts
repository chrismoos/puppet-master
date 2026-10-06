import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

async function openBoard(page: Page): Promise<void> {
  await logIn(page);
  await page.locator(".sb-board-link").click();
  await expect(page.locator(".workbench")).toBeVisible();
}

async function expectBoardTypographyAndContainment(page: Page): Promise<void> {
  const result = await page.locator(".workbench").evaluate((root) => {
    const visibleText = [...root.querySelectorAll<HTMLElement>("*")].filter((element) => {
      const style = getComputedStyle(element);
      const hasDirectText = [...element.childNodes].some((node) => node.nodeType === Node.TEXT_NODE && node.textContent?.trim());
      return hasDirectText && style.display !== "none" && style.visibility !== "hidden";
    });
    const undersized = visibleText
      .map((element) => ({ element: element.tagName.toLowerCase(), className: element.className, text: element.innerText.slice(0, 40), size: parseFloat(getComputedStyle(element).fontSize) }))
      .filter(({ size }) => size < 11);
    const overflowing = [root, ...root.querySelectorAll<HTMLElement>(".workbench-head,.workbench-metrics,.workbench-layout,.workbench-index,.workbench-inspector,.inspector-grid,.workflow-panel,.capture-panel")]
      .filter((element) => element.scrollWidth > element.clientWidth + 1 && getComputedStyle(element).overflowX !== "auto")
      .map((element) => ({ className: element.className, clientWidth: element.clientWidth, scrollWidth: element.scrollWidth }));
    const clipped = [...root.querySelectorAll<HTMLElement>(".workbench-metrics > button,.workbench-metrics > div,.workbench-group h2,.workbench-row,.workflow-status button,.workflow-priority button,.execution-card")]
      .filter((element) => element.scrollHeight > element.clientHeight + 1)
      .map((element) => ({ className: element.className, clientHeight: element.clientHeight, scrollHeight: element.scrollHeight }));
    const tokens = ["--board-type-micro", "--board-type-label", "--board-type-body", "--board-type-heading", "--board-type-title"]
      .map((name) => parseFloat(getComputedStyle(document.documentElement).getPropertyValue(name)));
    const metricsOverflow = getComputedStyle(root.querySelector<HTMLElement>(".workbench-metrics")!).overflowX;
    return { undersized, overflowing, clipped, tokens, metricsOverflow };
  });
  expect(result.tokens).toEqual([11, 12, 13, 15, 18]);
  expect(result.undersized).toEqual([]);
  expect(result.overflowing).toEqual([]);
  expect(result.clipped).toEqual([]);
  expect(result.metricsOverflow).toBe("auto");
}

test("opening the board queries once and keeps the pages it has loaded", async ({ page }) => {
  const firstPages: string[] = [];
  await page.route(/\/api\/buckets\/\d+\/items\?/, async (route) => {
    const url = new URL(route.request().url());
    if (url.searchParams.get("offset") === "0") {
      firstPages.push(url.search);
      // Outlast the board's own filter debounce so a repeated initial query
      // would already have been issued by the time the first page paints.
      await new Promise((resolve) => setTimeout(resolve, 600));
    }
    await route.continue();
  });

  await openBoard(page);
  const rows = page.locator(".workbench-row");
  await expect(rows).toHaveCount(50);
  expect(firstPages).toHaveLength(1);

  await page.getByRole("button", { name: "load 50 more" }).click();
  await expect(rows).toHaveCount(75);
  await page.getByRole("searchbox", { name: "search work items" }).fill("volume item 7");
  await expect(rows).toHaveCount(5);
  expect(firstPages).toHaveLength(2);
});

test("Workbench handles high-volume search, create, edit, workflow, reply, undo, error, and narrow desktop", async ({ page }) => {
  await openBoard(page);
  const rows = page.locator(".workbench-row");
  const indexStatus = page.locator(".workbench-index-status");
  await expect(rows).toHaveCount(50);
  await expect(indexStatus).toContainText("50 shown · 25 not loaded");
  await expect(page.getByRole("button", { name: "load 50 more" })).toBeVisible();
  await page.getByRole("button", { name: "load 50 more" }).click();
  await expect(rows).toHaveCount(75);
  await expect(indexStatus).toContainText("75 issues");
  await expect(indexStatus).toHaveCSS("position", "sticky");
  const results = page.locator(".workbench-index-results");
  // Measure both boxes in one frame: a live re-render between two separate
  // measurements detaches the row and yields no box at all.
  await expect.poll(() => results.evaluate((element) => {
    element.scrollTop = element.scrollHeight;
    const last = [...element.querySelectorAll<HTMLElement>(".workbench-row")].at(-1);
    if (!last) return null;
    return last.getBoundingClientRect().bottom <= element.getBoundingClientRect().bottom + 1;
  }), { message: "the last row must sit inside the scrolled results viewport" }).toBe(true);

  const search = page.getByRole("searchbox", { name: "search work items" });
  await search.fill("volume item 74");
  await expect(rows).toHaveCount(1);
  await expect(rows).toContainText("Volume item 74");
  await expect(indexStatus).toContainText("1 matches · 74 hidden");
  await expect.poll(() => page.url()).toContain("q=volume+item+74");
  await page.reload();
  await expect(search).toHaveValue("volume item 74");
  await expect(rows).toHaveCount(1);
  await search.fill("nothing can match this phrase");
  await expect(rows).toHaveCount(0);
  await expect(indexStatus).toContainText("0 matches · 75 hidden");
  await page.getByRole("button", { name: "clear filters" }).click();
  await expect(rows).toHaveCount(50);

  const metrics = page.getByRole("group", { name: "board summary" });
  await expect(metrics.getByRole("button")).toHaveCount(6);
  await expect(metrics).not.toContainText("results loaded");
  await expect(metrics).not.toContainText("server-ranked");
  await expect(metrics.getByRole("button", { name: /Filter to needs you items/ })).not.toHaveClass(/is-attention/);
  await expect(metrics.getByRole("button", { name: /Filter to planned items, 75 in bucket/ })).not.toContainText("bucket");
  await expect(page.getByRole("button", { name: "new item" }).locator("..")).toHaveClass(/workbench-filters/);
  await page.getByLabel("filter by project").selectOption("1");
  await page.getByLabel("filter by priority").selectOption("low");
  await search.fill("volume item 7");
  const plannedMetric = metrics.getByRole("button", { name: /Filter to planned items/ });
  await plannedMetric.click();
  await expect(rows).toHaveCount(5);
  await expect(metrics.getByRole("button", { name: /Clear planned items/ })).toHaveAttribute("aria-pressed", "true");
  await expect.poll(() => page.url()).toContain("q=volume+item+7");
  await expect.poll(() => page.url()).toContain("project=1");
  await expect.poll(() => page.url()).toContain("priority=low");
  await expect.poll(() => page.url()).toContain("summary=planned");
  await page.reload();
  const restoredPlanned = page.getByRole("group", { name: "board summary" }).getByRole("button", { name: /Clear planned items/ });
  await expect(restoredPlanned).toHaveAttribute("aria-pressed", "true");
  await restoredPlanned.focus();
  await restoredPlanned.press("Enter");
  await expect.poll(() => page.url()).not.toContain("summary=");
  await expect.poll(() => page.url()).toContain("priority=low");
  await page.getByRole("group", { name: "board summary" }).getByRole("button", { name: /Filter to items with live linked sessions/ }).click();
  await expect(rows).toHaveCount(0);
  await expect.poll(() => page.url()).toContain("summary=live_linked");
  await page.getByRole("button", { name: "clear filters" }).click();
  await expect(rows).toHaveCount(50);

  await page.getByRole("button", { name: "new item" }).click();
  const capture = page.getByRole("dialog", { name: "capture work" });
  const description = capture.getByLabel("description");
  await capture.getByLabel("title").fill("Workbench browser workflow");
  await description.fill("😀".repeat(65_536));
  await expect(capture.getByText("65,536 / 65,536 characters")).toBeVisible();
  await expect(capture.getByRole("button", { name: "create item" })).toBeEnabled();
  await description.fill(`${await description.inputValue()}😀`);
  await expect(capture.getByRole("alert")).toContainText("65,537 characters; the limit is 65,536");
  await expect(capture.getByRole("button", { name: "create item" })).toBeDisabled();
  await description.fill("Created by a human from the production board.");
  await capture.getByLabel(/open question/).fill("Should this ship in Wave 4?");
  await capture.getByRole("button", { name: "create item" }).click();
  await expect(capture).toBeHidden();
  await expect(rows.filter({ hasText: "Workbench browser workflow" })).toHaveCount(1);
  await rows.filter({ hasText: "Workbench browser workflow" }).click();

  const title = page.getByRole("textbox", { name: "item title" });
  await title.fill("Workbench browser workflow edited"); await title.blur();
  await expect(page.locator(".save-state")).toContainText(/saved|saving/);
  await page.getByRole("button", { name: "edit", exact: true }).click();
  await page.locator(".inspector-section textarea").first().fill("Edited master/detail description.");
  await page.getByRole("button", { name: "save description" }).click();
  await page.locator(".workflow-priority").getByRole("button", { name: "urgent" }).click();
  await page.locator(".workflow-status").getByRole("button", { name: "in progress" }).click();
  await expect(page.locator(".inspector-title-actions").getByRole("button", { name: "spawn session" })).toBeVisible();
  await page.getByRole("button", { name: "spawn session" }).click();
  await expect(page.locator(".modal")).toBeVisible();
  await page.locator(".modal").getByRole("button", { name: "cancel" }).click();

  await page.locator(".inspector-question textarea").fill("Yes, ship the Workbench.");
  await page.getByTitle("reply route").click();
  await page.getByRole("menuitem", { name: "Reply only (no session)" }).click();
  await expect(page.locator(".inspector-question")).toBeHidden();

  const toast = page.locator(".workbench-toast");
  await page.locator(".done-btn").click();
  await expect(toast).toContainText("Marked done");
  // Wait for the mutation's atomic row/facet query before asserting visibility.
  await expect(indexStatus).toContainText("1 hidden");
  const completedRow = rows.filter({ hasText: "Workbench browser workflow edited" });
  const completedToggle = page.locator(".workbench-group.is-completed h2 button");
  await expect(page.getByLabel("done", { exact: true })).toBeChecked();
  await expect(completedToggle).toHaveAttribute("aria-expanded", "false");
  await expect(completedRow).toHaveCount(0);
  await completedToggle.click();
  await expect(completedToggle).toHaveAttribute("aria-expanded", "true");
  await page.getByRole("button", { name: "load 50 more" }).click();
  await expect(completedRow).toHaveCount(1);
  await page.reload();
  await expect(page.getByRole("button", { name: /Collapse done and dropped items/ })).toHaveAttribute("aria-expanded", "true");
  await page.getByRole("button", { name: "load 50 more" }).click();
  await expect(completedRow).toHaveCount(1);
  await page.getByRole("button", { name: /Collapse done and dropped items/ }).click();
  await expect(completedRow).toHaveCount(0);

  await search.fill("Workbench browser workflow edited");
  await expect(completedRow).toHaveCount(1);
  await expect(page.getByRole("button", { name: /revealed for the current view/ })).toBeDisabled();
  await expect.poll(() => page.url()).toContain("q=Workbench+browser+workflow+edited");
  await page.getByRole("button", { name: "clear filters" }).click();
  await expect(completedRow).toHaveCount(0);
  await expect(page.getByRole("button", { name: /Expand done and dropped items/ })).toBeVisible();
  await page.getByLabel("filter by status").selectOption("done");
  await expect(completedRow).toHaveCount(1);
  await completedRow.click();
  await expect(title).toHaveValue("Workbench browser workflow edited");
  const itemPermalink = page.locator(".inspector-titlebar a");
  const itemHref = await itemPermalink.getAttribute("href");
  await itemPermalink.click();
  await expect.poll(() => page.url()).toContain(itemHref!.slice(1));
  await page.reload();
  await expect(page.getByRole("textbox", { name: "item title" })).toHaveValue("Workbench browser workflow edited");
  await expect(page.getByRole("button", { name: /revealed for the current view/ })).toBeVisible();
  await page.locator(".workflow-status").getByRole("button", { name: "planned" }).click();

  await page.getByRole("button", { name: "snooze 1d" }).click();
  await expect(toast).toContainText("Snoozed until tomorrow");
  await toast.getByRole("button", { name: "undo" }).click();

  await page.route("**/api/buckets/1/items?*", (route) => route.fulfill({ status: 500, contentType: "application/json", body: JSON.stringify({ error: "synthetic search failure" }) }));
  await search.fill("trigger error");
  await expect(page.locator(".board-route-view.is-active").getByRole("alert")).toContainText("synthetic search failure");
  await expect(indexStatus).toContainText("Issue count unavailable");
  await page.unroute("**/api/buckets/1/items?*");
  await page.getByRole("button", { name: "retry" }).click();
  await expect(page.getByText("Couldn’t load this view.")).toBeHidden();
  await page.getByRole("button", { name: "clear filters" }).click();
  await expect.poll(() => rows.count()).toBeGreaterThanOrEqual(50);
  expect(await rows.count()).toBeLessThanOrEqual(51); // bounded page, plus the direct-route target when it is outside that page
  await expect(rows.filter({ hasText: "Workbench browser workflow edited" })).toHaveCount(1);

  await page.setViewportSize({ width: 900, height: 800 });
  const geometry = await page.locator(".workbench").evaluate((element) => ({ client: element.clientWidth, scroll: element.scrollWidth }));
  expect(geometry.scroll).toBeLessThanOrEqual(geometry.client);
  await expect(indexStatus).toBeVisible();
  const indexGeometry = await page.locator(".workbench-index").evaluate((element) => ({ client: element.clientWidth, scroll: element.scrollWidth }));
  expect(indexGeometry.scroll).toBeLessThanOrEqual(indexGeometry.client);
  await expect(page.locator(".workflow-panel")).toBeVisible();
});

test("Workbench title-save undo alert expires after ten seconds", async ({ page }) => {
  await openBoard(page);
  await page.clock.install();
  const title = page.getByRole("textbox", { name: "item title" });
  await title.fill(`${await title.inputValue()} edited`);
  await title.blur();
  const toast = page.locator(".workbench-toast");
  await expect(toast).toContainText("Title saved");
  await page.clock.runFor(10_000);
  await expect(toast).toBeHidden();
});

test("Workbench footer distinguishes loading and unknown totals", async ({ page }) => {
  await openBoard(page);
  const indexStatus = page.locator(".workbench-index-status");
  await page.route("**/api/buckets/1/items?*", async (route) => {
    if (route.request().url().includes("q=delayed")) {
      await new Promise((resolve) => setTimeout(resolve, 700));
    }
    const response = await route.fetch();
    const body = await response.json() as Record<string, unknown>;
    delete body.counts;
    await route.fulfill({ response, json: body });
  });

  const search = page.getByRole("searchbox", { name: "search work items" });
  await search.fill("volume");
  await expect(indexStatus).toContainText("50 shown · more available");
  await search.fill("delayed");
  await expect(indexStatus).toContainText("Finding issues…");
  await expect(indexStatus).toContainText("0 matches");
});

test("Board type scale stays readable and contained at narrow widths and 200% zoom", async ({ page }) => {
  await openBoard(page);
  await page.locator(".workbench-row").first().click();
  await expect(page.locator(".inspector-grid")).toBeVisible();

  for (const width of [1440, 1100, 900, 760]) {
    await page.setViewportSize({ width, height: 1000 });
    await expectBoardTypographyAndContainment(page);
  }

  // Browser zoom reflows against the reduced CSS viewport. This is the
  // 1440x1000 desktop viewport as exposed to layout at 200% page zoom.
  await page.setViewportSize({ width: 720, height: 500 });
  await expectBoardTypographyAndContainment(page);

  await page.getByRole("button", { name: "new item" }).click();
  await expect(page.getByRole("dialog", { name: "capture work" })).toBeVisible();
  await expectBoardTypographyAndContainment(page);
});
