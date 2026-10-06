import { expect, test } from "./fixtures";
import { apiHeaders, logIn, openSettings } from "./support";

const ENROLLED = 27;
const DIALED = 9;
const PAGE_SIZE = 25;

test("Workers filters and pages a long list with the built-in worker first", async ({ page }) => {
  await logIn(page);
  const headers = { ...(await apiHeaders(page)), "content-type": "application/json" };
  for (let index = 1; index <= ENROLLED; index++) {
    const response = await page.request.post(`${process.env.PM_E2E_BASE_URL!}/api/workers/enroll`, {
      headers,
      data: JSON.stringify({
        label: `box-${String(index).padStart(2, "0")}`,
        ...(index === DIALED ? { connect_mode: "accept", endpoint: "10.0.0.9:7677" } : {}),
      }),
    });
    expect(response.ok()).toBe(true);
  }
  await openSettings(page, "Workers");

  const total = ENROLLED + 1;
  const rows = page.locator(".worker-row");
  const pager = page.locator(".ui-pager");
  await expect(rows).toHaveCount(PAGE_SIZE);
  await expect(rows.first()).toContainText("this controller");
  await expect(pager).toContainText(`1–${PAGE_SIZE} of ${total} workers`);
  await expect(pager).toContainText("Page 1 of 2");
  await expect(pager.getByRole("button", { name: "Previous" })).toBeDisabled();

  await pager.getByRole("button", { name: "Next" }).click();
  await expect(rows).toHaveCount(total - PAGE_SIZE);
  await expect(rows.last()).toContainText(`box-${ENROLLED}`);
  await expect(pager).toContainText(`${PAGE_SIZE + 1}–${total} of ${total} workers`);

  // Filtering starts over from the first page, and a short result needs no pager.
  const filter = page.getByRole("searchbox", { name: "Filter workers" });
  await filter.fill("controller dials");
  await expect(rows).toHaveCount(1);
  await expect(rows).toContainText("box-09");
  await expect(page.locator(".set-list-tools")).toContainText(`1 of ${total}`);
  await expect(pager).toHaveCount(0);
  await filter.fill("this controller");
  await expect(rows).toHaveCount(1);

  await filter.fill("no such worker");
  await expect(rows).toHaveCount(0);
  await expect(page.getByText("No worker matches that filter")).toBeVisible();
  await page.getByRole("button", { name: "Clear filter" }).click();
  await expect(filter).toHaveValue("");
  await expect(rows).toHaveCount(PAGE_SIZE);

  await pager.getByRole("combobox", { name: "Rows per page" }).selectOption("50");
  await expect(rows).toHaveCount(total);
  await expect(pager).toContainText("Page 1 of 1");
});
