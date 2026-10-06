import { expect, test, type Locator, type Page } from "./fixtures";
import { logIn } from "./support";

const VIEWPORTS = [
  { width: 1440, height: 1000 },
  { width: 360, height: 800 },
];
const LAYOUT_TOLERANCE_PX = 1;

async function expectContained(modal: Locator): Promise<void> {
  const metrics = await modal.evaluate((element) => {
    const rect = element.getBoundingClientRect();
    return {
      clientWidth: element.clientWidth,
      scrollWidth: element.scrollWidth,
      left: rect.left,
      right: rect.right,
      viewportWidth: document.documentElement.clientWidth,
    };
  });

  expect(metrics.scrollWidth).toBeLessThanOrEqual(metrics.clientWidth);
  expect(metrics.left).toBeGreaterThanOrEqual(0);
  expect(metrics.right).toBeLessThanOrEqual(metrics.viewportWidth);
}

async function expectContainedAtSupportedWidths(page: Page, modal: Locator): Promise<void> {
  for (const viewport of VIEWPORTS) {
    await page.setViewportSize(viewport);
    await expectContained(modal);
  }
}

/// The sticky actions row paints an opaque panel over whatever it covers,
/// so the distance from its top edge — including the pseudo-element that
/// extends the cover up over the flex gap — down to the lowest control
/// says how much of the dialog's own content the row is hiding. A focused
/// control reaches as far as its focus ring, which is drawn outside its box.
async function actionsCoverOfContent(modal: Locator): Promise<number> {
  return modal.evaluate((element) => {
    const actions = element.querySelector(".modal-actions")!;
    const coverHeight = Number.parseFloat(getComputedStyle(actions, "::before").height || "0");
    const coverTop = actions.getBoundingClientRect().top - coverHeight;
    const bottoms = [...element.querySelectorAll("input, select, textarea, .field-label, .modal-title")]
      .map((control) => {
        const style = getComputedStyle(control);
        const ring = style.outlineStyle === "none"
          ? 0
          : Number.parseFloat(style.outlineWidth) + Number.parseFloat(style.outlineOffset);
        return control.getBoundingClientRect().bottom + ring;
      });
    return Math.max(Number.NEGATIVE_INFINITY, ...bottoms) - coverTop;
  });
}

async function actionsBottomInset(modal: Locator): Promise<number> {
  return modal.evaluate((element) => {
    const actions = element.querySelector(".modal-actions")!;
    const scrollportBottom = element.getBoundingClientRect().top + element.clientTop + element.clientHeight;
    return scrollportBottom - actions.getBoundingClientRect().bottom;
  });
}

test("spawn role and advanced Worker restriction stay contained", async ({ page }) => {
  await logIn(page);
  // The ＋ opens the quick popover; the full dialog stays on the row menu.
  await page.locator(".sb-bucket-row").first().getByTitle(/actions for /).click();
  await page.getByRole("menuitem", { name: "new session…" }).click();

  const modal = page.locator(".modal");
  const role = modal.getByLabel("role");
  await expect(role).toHaveValue("1");
  await modal.locator(".spawn-advanced summary").click();
  const apiRows = modal.locator(".spawn-items-api");
  for (const viewport of VIEWPORTS) {
    await page.setViewportSize(viewport);
    await expectContained(modal);

    const rows = await apiRows.evaluateAll((elements) => elements.map((element) => {
      const input = element.querySelector("input")!;
      const rowRect = element.getBoundingClientRect();
      const inputRect = input.getBoundingClientRect();
      const style = getComputedStyle(element);
      return {
        clientWidth: element.clientWidth,
        scrollWidth: element.scrollWidth,
        height: rowRect.height,
        inputOffset: inputRect.top - rowRect.top,
        lineHeight: Number.parseFloat(style.lineHeight),
      };
    }));

    expect(rows).toHaveLength(1);
    for (const row of rows) {
      expect(row.scrollWidth).toBeLessThanOrEqual(row.clientWidth);
      expect(row.height).toBeGreaterThan(row.lineHeight);
      expect(row.inputOffset).toBeLessThan(row.lineHeight);
    }
  }

  await role.selectOption("2");
  await expect(modal.getByText("bucket-level authority", { exact: false })).toBeVisible();
  await expect(modal.locator(".spawn-items-api")).toHaveCount(0);

  await modal.getByRole("button", { name: "cancel" }).click();
});

test("workspace modals remain contained at normal and narrow widths", async ({ page }) => {
  await logIn(page);
  await page.getByTitle("new workspace").click();

  const modal = page.locator(".modal");
  await expectContainedAtSupportedWidths(page, modal);
  await modal.getByRole("button", { name: "cancel" }).click();

  await page.setViewportSize(VIEWPORTS[0]);
  await page.getByTitle("new workspace").click();
  await modal.locator("input").fill("modal layout check");
  await modal.getByRole("button", { name: "create workspace" }).click();
  await expect(modal).toBeHidden();

  await page.getByRole("button", { name: "Delete modal layout check" }).click();
  await expectContainedAtSupportedWidths(page, modal);
  await modal.getByRole("button", { name: "delete workspace" }).click();
  await expect(modal).toBeHidden();
});

test("instruction management auto-loads history and supports preview and optimistic save", async ({ page }) => {
  await logIn(page);
  await page.goto(`${process.env.PM_E2E_BASE_URL!}#/settings/instructions`);
  const panel = page.locator(".instructions-panel");
  // Bucket, scope and role sit in one bar above the editor.
  const bar = panel.locator(".set-layer-bar");
  await expect(bar.getByRole("combobox", { name: "Bucket" })).toHaveValue("1");
  await expect(bar.getByRole("combobox", { name: "Scope" })).toHaveValue("");
  const role = bar.getByRole("group", { name: "Applies to" });
  await expect(role.getByRole("button")).toHaveText(["Everyone", "Workers", "Supervisors"]);
  await expect(role.getByRole("button", { name: "Everyone" })).toHaveAttribute("aria-pressed", "true");

  // Write and the two previews are tabs on the editor.
  const tabs = panel.getByRole("tablist", { name: "Editor view" });
  await expect(tabs.getByRole("tab")).toHaveText(["Write", "Preview as worker", "Preview as supervisor"]);
  await expect(tabs.getByRole("tab", { name: "Write" })).toHaveAttribute("aria-selected", "true");
  await expect(tabs).toContainText("new layer, not saved yet");

  // The revision note and Save sit in the editor footer.
  const save = panel.locator(".set-editor > footer").getByRole("button", { name: "Save revision" });
  await expect(save).toBeDisabled();
  await panel.getByRole("textbox", { name: "Instructions" }).fill("Browser durable policy");
  await panel.locator(".set-editor > footer").getByRole("textbox", { name: "Revision note" }).fill("first policy");
  await save.click();
  await expect(tabs).toContainText("revision 1");

  // History is below the editor, each revision with its note and a revert.
  const revision = panel.locator(".instruction-revision");
  await expect(revision).toHaveCount(1);
  await expect(revision.getByText("r1", { exact: true })).toBeVisible();
  await expect(revision).toContainText("first policy");
  await expect(revision.getByRole("button", { name: "Revert to revision 1" })).toBeVisible();
  expect((await revision.boundingBox())!.y).toBeGreaterThan((await panel.locator(".set-editor").boundingBox())!.y);

  await tabs.getByRole("tab", { name: "Preview as worker" }).click();
  await expect(panel.locator(".instruction-preview")).toContainText("Browser durable policy");
  await expect(panel.getByRole("textbox", { name: "Instructions" })).toHaveCount(0);
  await tabs.getByRole("tab", { name: "Preview as supervisor" }).click();
  await expect(panel.locator(".instruction-preview")).toContainText("Browser durable policy");
  await tabs.getByRole("tab", { name: "Write" }).click();
  await expect(panel.getByRole("textbox", { name: "Instructions" })).toHaveValue("Browser durable policy");
});

test("the workspace dialog's actions row clears the name field and its focus ring", async ({ page }) => {
  await logIn(page);
  await page.getByTitle("new workspace").click();
  const modal = page.locator(".modal");
  await expect(modal.locator("input")).toBeFocused();

  for (const viewport of VIEWPORTS) {
    await page.setViewportSize(viewport);
    expect(await modal.evaluate((element) => element.scrollHeight > element.clientHeight)).toBe(false);
    expect(await actionsCoverOfContent(modal)).toBeLessThanOrEqual(0);
    expect(Math.abs(await actionsBottomInset(modal))).toBeLessThanOrEqual(LAYOUT_TOLERANCE_PX);
  }

  await modal.getByRole("button", { name: "cancel" }).click();
});

test("a scrolling dialog pins its actions row and still scrolls every control clear of it", async ({ page }) => {
  await logIn(page);
  await page.locator(".sb-bucket-row").first().getByTitle(/actions for /).click();
  await page.getByRole("menuitem", { name: "new session…" }).click();

  const modal = page.locator(".modal");
  await modal.locator(".spawn-advanced summary").click();
  await page.setViewportSize({ width: 900, height: 460 });
  await expect
    .poll(() => modal.evaluate((element) => element.scrollHeight > element.clientHeight))
    .toBe(true);

  // Unscrolled, the row is pinned to the bottom of the scrollport and the
  // content it covers is the content still below the fold.
  expect(Math.abs(await actionsBottomInset(modal))).toBeLessThanOrEqual(LAYOUT_TOLERANCE_PX);
  expect(await actionsCoverOfContent(modal)).toBeGreaterThan(0);

  await modal.evaluate((element) => { element.scrollTop = element.scrollHeight; });
  expect(Math.abs(await actionsBottomInset(modal))).toBeLessThanOrEqual(LAYOUT_TOLERANCE_PX);
  expect(await actionsCoverOfContent(modal)).toBeLessThanOrEqual(0);

  await modal.getByRole("button", { name: "cancel" }).click();
});
