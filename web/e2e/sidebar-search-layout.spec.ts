import { expect, test, type Locator } from "./fixtures";
import { logIn } from "./support";

type SearchGeometry = {
  inputTop: number;
  inputBottom: number;
  inputRight: number;
  searchBottom: number;
  statusTop: number;
  statusBottom: number;
  statusHeight: number;
  statusLeft: number;
  firstBucketTop: number;
  borderBottomWidth: string;
};

/**
 * The count sits in the search row beside the input rather than on a reserved
 * line beneath it, so the row height no longer depends on whether a count is
 * showing and the list below it cannot move.
 */
const SEARCH_ROW_BELOW_INPUT_PX = 7;

async function searchGeometry(sidebar: Locator): Promise<SearchGeometry> {
  return sidebar.evaluate((element) => {
    const input = element.querySelector<HTMLInputElement>("#session-search")!;
    const search = element.querySelector<HTMLElement>(".sb-search")!;
    const status = search.querySelector<HTMLElement>("[role=status]")!;
    const firstBucket = element.querySelector<HTMLElement>(".sb-scroll > .sb-bucket")!;
    const inputBox = input.getBoundingClientRect();
    const searchBox = search.getBoundingClientRect();
    const statusBox = status.getBoundingClientRect();
    return {
      inputTop: inputBox.top,
      inputBottom: inputBox.bottom,
      inputRight: inputBox.right,
      searchBottom: searchBox.bottom,
      statusTop: statusBox.top,
      statusBottom: statusBox.bottom,
      statusHeight: statusBox.height,
      statusLeft: statusBox.left,
      firstBucketTop: firstBucket.getBoundingClientRect().top,
      borderBottomWidth: getComputedStyle(search).borderBottomWidth,
    };
  });
}

test("session search reports its count inline without reserving a status line", async ({ page, isolatedDaemon }) => {
  await page.addInitScript(() => localStorage.setItem("pm.showEnded", "false"));
  await logIn(page);

  const sidebar = page.locator(".sidebar");
  const searchbox = page.getByRole("searchbox", { name: "Search sessions" });
  const status = page.locator(".sb-search [role=status]");

  // The live region stays in the accessibility tree while empty so a later
  // count is still announced.
  await expect(status).toHaveText("");
  await expect(status).toBeAttached();

  for (const width of [220, 304, 620]) {
    await page.locator(".workspace").evaluate((element, value) => {
      (element as HTMLElement).style.setProperty("--sidebar-w", `${value}px`);
    }, width);
    const geometry = await searchGeometry(sidebar);
    expect(geometry.statusHeight).toBe(0);
    expect(geometry.searchBottom - geometry.inputBottom).toBeCloseTo(SEARCH_ROW_BELOW_INPUT_PX, 0);
    expect(geometry.firstBucketTop - geometry.searchBottom).toBeCloseTo(0, 0);
    expect(geometry.borderBottomWidth).toBe("1px");
    const overflow = await sidebar.evaluate((element) => element.scrollWidth - element.clientWidth);
    expect(overflow).toBeLessThanOrEqual(0);
  }

  await searchbox.focus();
  await expect(searchbox).toBeFocused();
  await expect(searchbox).toHaveCSS("outline-width", "2px");

  const empty = await searchGeometry(sidebar);

  await searchbox.fill("browser-e2e");
  await expect(status).toHaveText("2 results");
  const populated = await searchGeometry(sidebar);

  // The count shares the input's line rather than sitting under it.
  expect(populated.statusHeight).toBeGreaterThan(0);
  expect(populated.statusTop).toBeGreaterThanOrEqual(populated.inputTop);
  expect(populated.statusBottom).toBeLessThanOrEqual(populated.inputBottom);
  expect(populated.statusLeft).toBeGreaterThanOrEqual(populated.inputRight);

  // The property the old reserved line existed to protect: showing a count
  // must not move the list.
  expect(populated.searchBottom).toBeCloseTo(empty.searchBottom, 0);
  expect(populated.firstBucketTop).toBeCloseTo(empty.firstBucketTop, 0);
  expect(populated.searchBottom - populated.inputBottom)
    .toBeCloseTo(SEARCH_ROW_BELOW_INPUT_PX, 0);

  await searchbox.fill("no-session-answers-to-this");
  await expect(status).toHaveText("no matches");
  const missing = await searchGeometry(sidebar);
  expect(missing.searchBottom).toBeCloseTo(empty.searchBottom, 0);
  expect(await sidebar.evaluate((element) => element.scrollWidth - element.clientWidth))
    .toBeLessThanOrEqual(0);

  await searchbox.fill("");
  await expect(status).toHaveText("");
  const cleared = await searchGeometry(sidebar);
  expect(cleared.statusHeight).toBe(0);
  expect(cleared.firstBucketTop).toBeCloseTo(empty.firstBucketTop, 0);

  isolatedDaemon.seedEndedSessions(40);
  await isolatedDaemon.restart();
  await page.reload();
  await page.getByLabel("show ended").check();
  await expect(page.locator(".sb-session")).toHaveCount(42);
  await expect(status).toHaveText("");
  const scroll = page.locator(".sb-scroll");
  const scrollMetrics = await scroll.evaluate((element) => ({
    clientHeight: element.clientHeight,
    scrollHeight: element.scrollHeight,
  }));
  expect(scrollMetrics.scrollHeight).toBeGreaterThan(scrollMetrics.clientHeight);
  await scroll.evaluate((element) => { element.scrollTop = 80; });
  await expect.poll(() => scroll.evaluate((element) => element.scrollTop)).toBeGreaterThan(0);

  await page.setViewportSize({ width: 720, height: 500 });
  const responsive = await sidebar.evaluate((element) => ({
    clientWidth: element.clientWidth,
    scrollWidth: element.scrollWidth,
  }));
  expect(responsive.scrollWidth).toBeLessThanOrEqual(responsive.clientWidth);
});
