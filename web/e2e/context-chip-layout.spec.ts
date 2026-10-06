import { expect, test, type Locator, type Page } from "./fixtures";
import { logIn } from "./support";

// The list reserves a scrollbar gutter, so the width at which a mixed chip row
// fits on one line sits a gutter further out than the raw row width suggests.
const SIDEBAR_SCROLLBAR_GUTTER = 8;
const SIDEBAR_WIDTHS = [220, 304, 620 + SIDEBAR_SCROLLBAR_GUTTER] as const;
const LONG_VALUE = "responsive-chip-value-that-uses-the-whole-available-session-row";

async function installFixture(page: Page): Promise<void> {
  await page.locator("#root").evaluate((root, longValue) => {
    const chip = (label: string, value: string, classes = "") => `
      <span class="ctx-chip ${classes}" title="${label}: ${value}">
        <span class="ctx-chip-label">${label}</span>
        <span class="ctx-chip-value">${value}</span>
      </span>`;
    const rows = (section: string, attention = false) => `
      <div class="sb-session-row ${attention ? "is-needs-input" : ""}">
        <button class="sb-session ${attention ? "is-needs-input" : ""}" type="button">
          <span class="sb-session-gutter"><span class="state-dot st-working"></span></span>
          <span class="sb-session-content">
            <span class="sb-session-title">${section} single long chip</span>
            <span class="sb-session-chips single-row">
              ${chip("status", longValue, "ctx-code")}
            </span>
            <span class="sb-session-meta"><span class="sb-session-elapsed">1m</span></span>
          </span>
        </button>
      </div>
      <div class="sb-session-row ${attention ? "is-needs-input" : ""}">
        <button class="sb-session ${attention ? "is-needs-input" : ""}" type="button">
          <span class="sb-session-gutter"><span class="state-dot st-idle"></span></span>
          <span class="sb-session-content">
            <span class="sb-session-title">${section} mixed chips</span>
            <span class="sb-session-chips mixed-row">
              ${chip("state", "ready")}
              ${chip("tests", "12", "ctx-good")}
              ${chip("branch", longValue, "ctx-code")}
            </span>
            <span class="sb-session-meta"><span class="sb-session-elapsed">2m</span></span>
          </span>
        </button>
      </div>
      <div class="sb-session-row ${attention ? "is-needs-input" : ""}">
        <button class="sb-session ${attention ? "is-needs-input" : ""}" type="button">
          <span class="sb-session-gutter"><span class="state-dot st-idle"></span></span>
          <span class="sb-session-content">
            <span class="sb-session-title">${section} value kinds</span>
            <span class="sb-session-chips kinds-row">
              ${chip("text", longValue)}
              ${chip("code", longValue, "ctx-code")}
              ${chip("badge", longValue, "ctx-warn")}
              ${chip("metric", longValue, "ctx-good")}
              <a class="ctx-chip ctx-url" href="https://example.invalid/${longValue}" title="url: https://example.invalid/${longValue}">
                <span class="ctx-chip-label">url</span><span class="ctx-chip-value">↗</span>
              </a>
              ${chip("progress", longValue, "ctx-progress")}
            </span>
            <span class="sb-session-meta"><span class="sb-session-elapsed">3m</span></span>
          </span>
        </button>
      </div>`;

    root.innerHTML = `
      <aside class="sidebar chip-layout-fixture">
        <div class="sb-scroll">
          <section class="normal-section">${rows("normal")}</section>
          <section class="attention-section">${rows("needs input", true)}</section>
        </div>
      </aside>
      <span class="ctx-chip legacy-width-ruler" aria-hidden="true"></span>`;
  }, LONG_VALUE);
}

async function setSidebarWidth(sidebar: Locator, width: number): Promise<void> {
  await sidebar.evaluate((element, nextWidth) => {
    (element as HTMLElement).style.width = `${nextWidth}px`;
  }, width);
}

test("context chips size to normal and in-place needs-input session rows", async ({ page }) => {
  await logIn(page);
  await installFixture(page);

  const sidebar = page.locator(".chip-layout-fixture");
  const sections = [page.locator(".normal-section"), page.locator(".attention-section")];
  const visibleValueWidths = new Map<string, number[]>();

  for (const width of SIDEBAR_WIDTHS) {
    await setSidebarWidth(sidebar, width);

    for (const section of sections) {
      const key = await section.getAttribute("class") ?? "section";
      const singleContainer = section.locator(".single-row");
      const singleChip = singleContainer.locator(".ctx-chip");
      const singleValue = singleChip.locator(".ctx-chip-value");
      const metrics = await singleValue.evaluate((element) => ({
        clientWidth: element.clientWidth,
        scrollWidth: element.scrollWidth,
      }));
      visibleValueWidths.set(key, [...(visibleValueWidths.get(key) ?? []), metrics.clientWidth]);

      expect(await singleChip.getAttribute("title")).toBe(`status: ${LONG_VALUE}`);
      expect(metrics.scrollWidth > metrics.clientWidth).toBe(width !== SIDEBAR_WIDTHS[2]);

      const containers = section.locator(".sb-session-chips");
      for (const container of await containers.all()) {
        const overflow = await container.evaluate((element) => ({
          clientWidth: element.clientWidth,
          scrollWidth: element.scrollWidth,
        }));
        expect(overflow.scrollWidth).toBeLessThanOrEqual(overflow.clientWidth);
      }

      const chips = section.locator(".ctx-chip");
      for (const chip of await chips.all()) {
        const contained = await chip.evaluate((element) => ({
          chipWidth: element.getBoundingClientRect().width,
          rowWidth: element.parentElement!.getBoundingClientRect().width,
        }));
        expect(contained.chipWidth).toBeLessThanOrEqual(contained.rowWidth);
      }

      const mixedChips = section.locator(".mixed-row .ctx-chip");
      const tops = await mixedChips.evaluateAll((elements) =>
        elements.map((element) => Math.round(element.getBoundingClientRect().top)),
      );
      if (width === SIDEBAR_WIDTHS[2]) {
        expect(new Set(tops).size).toBe(1);
      } else {
        expect(new Set(tops).size).toBeGreaterThan(1);
      }
    }
  }

  for (const widths of visibleValueWidths.values()) {
    expect(widths[0]).toBeLessThan(widths[1]);
    expect(widths[1]).toBeLessThan(widths[2]);
  }

  const wideChipWidth = await page.locator(".normal-section .single-row .ctx-chip").evaluate(
    (element) => element.getBoundingClientRect().width,
  );
  const legacyWidth = await page.locator(".legacy-width-ruler").evaluate((element) => {
    const ruler = element as HTMLElement;
    ruler.style.position = "fixed";
    ruler.style.width = "26ch";
    ruler.style.maxWidth = "none";
    return ruler.getBoundingClientRect().width;
  });
  expect(wideChipWidth).toBeGreaterThan(legacyWidth);
});
