import { execFileSync } from "node:child_process";
import { expect, test, type Page, type TestInfo } from "./fixtures";
import { computedColor, logIn } from "./support";

const TERMINAL_READY_TIMEOUT_MS = 20_000;

async function expectVisibleTerminalOnline(page: Page, sessionId: number): Promise<void> {
  await expect.poll(() => page.evaluate((expectedKey) => {
    const stage = (window as Window & {
      __pmStage?: {
        debugSnapshot(): Array<{
          key: string;
          visible: boolean;
          socket: { phase: "online" | "reconnecting" } | null;
        }>;
        layers: Map<string, {
          term: { buffer: { active: {
            length: number;
            getLine(line: number): { translateToString(trimRight?: boolean): string } | undefined;
          } } };
        }>;
      };
    }).__pmStage;
    const debug = stage?.debugSnapshot().find((layer) => layer.visible && layer.key === expectedKey);
    const layer = debug ? stage?.layers.get(debug.key) : undefined;
    if (debug?.socket?.phase !== "online" || !layer) return false;
    const buffer = layer.term.buffer.active;
    for (let row = 0; row < buffer.length; row += 1) {
      if ((buffer.getLine(row)?.translateToString(true) ?? "").includes("READY ")) return true;
    }
    return false;
  }, `s:${sessionId}`), {
    message: "waiting for the selected test agent to announce readiness",
    timeout: TERMINAL_READY_TIMEOUT_MS,
  }).toBe(true);
}

async function installFixture(page: Page): Promise<void> {
  await page.locator("#root").evaluate((root) => {
    root.innerHTML = `
      <aside class="sidebar sidebar-layout-fixture" style="width:304px">
        <div class="sb-scroll">
          <div class="sb-bucket">
          <div class="sb-loose">
          <div class="sb-session-row is-selected">
            <div class="sb-session-main">
              <button class="sb-session is-selected" type="button" aria-label="Trace session to board and preserve the same terminal viewport without recreation, working">
                <span class="sb-session-gutter"><span class="state-dot st-working" role="img" aria-label="working"></span></span>
                <span class="sb-session-content">
                  <span class="sb-session-title">Trace session to board and preserve the same terminal viewport without recreation</span>
                  <span class="sb-session-chips"><span class="ctx-chip"><span class="ctx-chip-label">branch</span><span class="ctx-chip-value">item-64-sidebar-layout</span></span></span>
                  <span class="sb-session-meta">
                    <span class="sb-session-role is-worker" role="img" aria-label="Worker role — executes a scoped task in a project" data-tooltip="Worker role — executes a scoped task in a project"></span>
                    <span class="sb-session-elapsed is-now" aria-label="Last activity: now">now</span>
                  </span>
                </span>
              </button>
              <span class="sb-linked-item-row"><a class="sb-linked-item" href="#/bucket/1/item/64"><span class="sb-linked-item-ref">#64</span><span class="sb-linked-item-separator">·</span><span class="sb-linked-item-title">Sidebar session layout implementation</span></a></span>
            </div>
          </div>
            <div class="sb-session-row is-needs-input is-attention-unseen">
              <div class="sb-session-main">
                <button class="sb-session is-needs-input" type="button" aria-label="Confirm the migration compatibility decision, needs input">
                  <span class="sb-session-gutter"><span class="state-dot st-needs-input" role="img" aria-label="needs input"></span></span>
                  <span class="sb-session-content">
                    <span class="sb-session-title">Confirm the migration compatibility decision</span>
                    <span class="sb-session-meta"><span class="sb-session-elapsed" aria-label="Last activity: 38s ago">38s</span></span>
                  </span>
                </button>
              </div>
              <button class="sb-menu-btn" type="button" aria-label="actions for Confirm the migration compatibility decision">⋯</button>
            </div>
            <div class="sb-session-row is-needs-input is-attention-unseen is-selected">
              <div class="sb-session-main">
                <button class="sb-session is-needs-input is-selected" type="button" aria-label="Choose the narrow layout fallback, needs input" aria-current="page">
                  <span class="sb-session-gutter"><span class="state-dot st-needs-input" role="img" aria-label="needs input"></span></span>
                  <span class="sb-session-content">
                    <span class="sb-session-title">Choose the narrow layout fallback</span>
                    <span class="sb-session-detail">Remote host offline</span>
                    <span class="sb-session-meta"><span class="sb-session-elapsed is-muted" aria-label="Session inactive">inactive</span></span>
                  </span>
                </button>
              </div>
            </div>
          </div>
          </div>
        </div>
      </aside>`;
  });
}

async function requestInput(page: Page, title: string, sessionId: number): Promise<void> {
  const row = page.locator(".sb-session-row").filter({
    has: page.locator(".sb-session-title", { hasText: new RegExp(`^${title}$`) }),
  });
  await row.locator(".sb-session").click();
  const terminal = page.locator(".term-layer:visible .xterm-helper-textarea");
  await expect(terminal).toBeAttached();
  await expectVisibleTerminalOnline(page, sessionId);
  await terminal.pressSequentially("needs");
  await terminal.press("Enter");
  await expect(row).toHaveClass(/is-needs-input/);
}

test("multiple NeedsInput sessions stay in place through navigation and collapse", async ({ page, isolatedDaemon }) => {
  await logIn(page);
  await requestInput(page, "browser-e2e", isolatedDaemon.session("browser-e2e")!.id);
  await requestInput(page, "browser-e2e-two", isolatedDaemon.session("browser-e2e-two")!.id);

  const attentionRows = page.locator(".sb-scroll .sb-session-row.is-needs-input");
  await expect(attentionRows).toHaveCount(2);
  await expect(page.locator(".sb-pinned")).toHaveCount(0);
  await expect(page.getByRole("button", { name: "browser-e2e, needs input" })).toHaveCount(1);
  await expect(page.getByRole("button", { name: "browser-e2e-two, needs input" })).toHaveCount(1);
  expect(await attentionRows.evaluateAll((rows) => rows.every((row) => row.closest(".sb-bucket") !== null))).toBe(true);

  const selected = page.getByRole("button", { name: "browser-e2e-two, needs input" });
  await expect(selected).toHaveAttribute("aria-current", "page");
  await page.getByRole("button", { name: "browser-e2e, needs input" }).click();
  await expect(page.getByRole("button", { name: "browser-e2e, needs input" })).toHaveAttribute("aria-current", "page");
  await expect(selected).not.toHaveAttribute("aria-current", "page");

  const bucketToggle = page.locator(".sb-bucket-head").filter({ hasText: "browser-e2e" });
  await bucketToggle.click();
  await expect(bucketToggle).toHaveAttribute("aria-expanded", "false");
  await expect(page.locator(".sb-scroll .sb-session-row.is-needs-input")).toHaveCount(0);
  await bucketToggle.click();
  await expect(page.locator(".sb-scroll .sb-session-row.is-needs-input")).toHaveCount(2);

  await page.setViewportSize({ width: 1100, height: 700 });
  await page.locator("html").evaluate((element) => element.style.setProperty("--sidebar-w", "220px"));
  const overflow = await page.locator(".sidebar").evaluate((element) => ({
    clientWidth: element.clientWidth,
    scrollWidth: element.scrollWidth,
  }));
  expect(overflow.scrollWidth).toBeLessThanOrEqual(overflow.clientWidth);
});

test("headline-first rows preserve restrained selection and supporting hierarchy", async ({ page }, testInfo: TestInfo) => {
  await logIn(page);
  await installFixture(page);

  const sidebar = page.locator(".sidebar-layout-fixture");
  for (const width of [220, 304, 620]) {
    await sidebar.evaluate((element, value) => { (element as HTMLElement).style.width = `${value}px`; }, width);
    const metrics = await sidebar.locator(".sb-session-row").first().evaluate((row) => {
      const headline = row.querySelector<HTMLElement>(".sb-session-title")!;
      const item = row.querySelector<HTMLElement>(".sb-linked-item")!;
      const meta = row.querySelector<HTMLElement>(".sb-session-meta")!;
      const selected = row.querySelector<HTMLElement>(".sb-session")!;
      return {
        headlineSize: Number.parseFloat(getComputedStyle(headline).fontSize),
        itemSize: Number.parseFloat(getComputedStyle(item).fontSize),
        metaSize: Number.parseFloat(getComputedStyle(meta).fontSize),
        headlineWidth: headline.getBoundingClientRect().width,
        contentWidth: row.querySelector<HTMLElement>(".sb-session-content")!.getBoundingClientRect().width,
        contentRightPadding: Number.parseFloat(getComputedStyle(row.querySelector<HTMLElement>(".sb-session-content")!).paddingRight),
        selectedBackground: getComputedStyle(selected).backgroundColor,
        gutterBackground: getComputedStyle(row.querySelector<HTMLElement>(".sb-session-gutter")!).backgroundColor,
        barWidth: getComputedStyle(row, "::before").width,
        overflow: row.scrollWidth - row.clientWidth,
      };
    });
    expect(metrics.headlineSize).toBe(14);
    expect(metrics.headlineSize).toBeGreaterThan(metrics.itemSize);
    expect(metrics.headlineSize).toBeGreaterThan(metrics.metaSize);
    expect(metrics.headlineWidth).toBeCloseTo(metrics.contentWidth - metrics.contentRightPadding, 0);
    expect(metrics.selectedBackground).toBe("rgba(85, 168, 255, 0.055)");
    expect(metrics.gutterBackground).toBe("rgba(0, 0, 0, 0)");
    expect(metrics.barWidth).toBe("2px");
    expect(metrics.overflow).toBeLessThanOrEqual(0);

    const alignment = await sidebar.locator(".sb-session-row").evaluateAll((rows) => rows.map((row) => ({
      dot: row.querySelector<HTMLElement>(".state-dot")!.getBoundingClientRect().left,
      headline: row.querySelector<HTMLElement>(".sb-session-title")!.getBoundingClientRect().left,
    })));
    expect(alignment[1].dot).toBeCloseTo(alignment[0].dot, 0);
    expect(alignment[1].headline).toBeCloseTo(alignment[0].headline, 0);
  }

  await expect(sidebar.locator(".sb-linked-item-status")).toHaveCount(0);
  await expect(sidebar.locator(".sb-session-elapsed")).toHaveText(["now", "38s", "inactive"]);
  await expect(sidebar.locator(".sb-session-elapsed", { hasText: "0s" })).toHaveCount(0);
  await expect(sidebar).not.toContainText("launch:");
  await expect(sidebar).not.toContainText("(*)");
  await expect(sidebar.locator(".sb-pinned")).toHaveCount(0);
  await expect(sidebar.locator(".sb-session-row.is-needs-input")).toHaveCount(2);
  await expect(sidebar.getByRole("button", { name: "Confirm the migration compatibility decision, needs input" })).toBeVisible();
  await expect(sidebar.getByRole("button", { name: "Choose the narrow layout fallback, needs input" })).toHaveAttribute("aria-current", "page");

  const attention = sidebar.locator(".sb-session-row.is-needs-input").first();
  expect(await computedColor(attention)).toEqual({ r: 255, g: 178, b: 36, a: 0.075 });
  await expect(attention.locator(".sb-session")).toHaveCSS("background-color", "rgba(0, 0, 0, 0)");
  await expect(attention.locator(".sb-menu-btn")).toHaveCSS("background-color", "rgba(0, 0, 0, 0)");
  await attention.hover();
  expect(await computedColor(attention)).toEqual({ r: 255, g: 178, b: 36, a: 0.12 });
  await attention.locator(".sb-session").focus();
  await expect(attention.locator(".sb-session")).toBeFocused();
  expect(await computedColor(attention)).toEqual({ r: 255, g: 178, b: 36, a: 0.12 });

  const selectedAttention = sidebar.locator(".sb-session-row.is-needs-input.is-selected");
  expect(await computedColor(selectedAttention, "outlineColor")).toEqual({ r: 77, g: 163, b: 255, a: 0.55 });
  expect(await selectedAttention.evaluate((row) => getComputedStyle(row, "::before").backgroundColor)).toBe("rgb(77, 163, 255)");
  await expect(selectedAttention.locator(".sb-session-detail")).toHaveCSS("color", "rgb(154, 164, 184)");
  // The detail's box spans the content column whatever its padding, so the
  // text itself is what has to line up with the title.
  const textLeft = (selector: string) => selectedAttention.locator(selector).evaluate((element) => {
    const range = document.createRange();
    range.selectNodeContents(element);
    return range.getClientRects()[0].left;
  });
  await expect(selectedAttention.locator(".sb-session-detail")).toHaveCSS("padding-left", "0px");
  await expect(selectedAttention.locator(".sb-session-detail")).toHaveCSS("font-size", "11.5px");
  expect(await textLeft(".sb-session-detail")).toBeCloseTo(await textLeft(".sb-session-title"), 0);
  await expect(selectedAttention.locator(".sb-session-elapsed")).toHaveCSS("color", "rgb(154, 164, 184)");

  await sidebar.evaluate((element) => { (element as HTMLElement).style.width = "304px"; });
  const desktopEvidence = "test-results/sidebar-session-desktop.png";
  await sidebar.screenshot({ path: desktopEvidence });
  await testInfo.attach("sidebar-desktop", {
    path: desktopEvidence,
    contentType: "image/png",
  });

  await page.setViewportSize({ width: 720, height: 500 });
  await sidebar.evaluate((element) => { (element as HTMLElement).style.width = "220px"; });
  const zoomMetrics = await sidebar.evaluate((element) => ({
    client: element.clientWidth,
    scroll: element.scrollWidth,
  }));
  expect(zoomMetrics.scroll).toBeLessThanOrEqual(zoomMetrics.client);
  const narrowEvidence = "test-results/sidebar-session-narrow.png";
  await sidebar.screenshot({ path: narrowEvidence });
  await testInfo.attach("sidebar-narrow-200-percent-equivalent", {
    path: narrowEvidence,
    contentType: "image/png",
  });
});

test("role explanation belongs only to direct icon hover, focus, and touch", async ({ page }) => {
  execFileSync(process.env.PM_E2E_PM_BIN!, [
    "spawn", "--project", "1", "--agent", "codex", "--title", "role-tooltip-supervisor",
    "--role", "supervisor",
  ], {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
  });
  await logIn(page, { minimumSessions: 3 });

  const workerRole = page.getByRole("img", { name: /Worker role/ }).first();
  const supervisorRole = page.getByRole("img", { name: /Supervisor role/ }).first();

  for (const role of [workerRole, supervisorRole]) {
    const wrapper = role.locator("..");
    const tooltip = wrapper.locator(".sb-session-role-tooltip");
    const row = role.locator("xpath=ancestor::button[contains(@class, 'sb-session')]");
    const box = await row.boundingBox();
    if (!box) throw new Error("session row is not visible");

    await row.click({ position: { x: box.width - 6, y: 4 } });
    await expect(row).toHaveAttribute("aria-current", "page");
    await expect(page.locator('.term-layer[style*="visibility: visible"] .xterm-helper-textarea')).toBeFocused();
    await expect(tooltip).toHaveCSS("opacity", "0");

    await row.locator(".sb-session-title").click();
    await expect(tooltip).toHaveCSS("opacity", "0");
    await row.locator(".sb-session-elapsed").click();
    await expect(tooltip).toHaveCSS("opacity", "0");

    await role.hover();
    await expect(tooltip).toHaveCSS("opacity", "1");
    await page.mouse.move(900, 700);
    await expect(tooltip).toHaveCSS("opacity", "0");

    await row.focus();
    await expect(tooltip).toHaveCSS("opacity", "0");
    await page.keyboard.press("Tab");
    await expect(role).toBeFocused();
    await expect(tooltip).toHaveCSS("opacity", "1");
    await expect(role).toHaveAttribute("aria-describedby", await tooltip.getAttribute("id"));
    await role.press("Escape");
    await expect(tooltip).toHaveCSS("opacity", "0");
    await expect(role).not.toBeFocused();
  }

  const workerTooltip = workerRole.locator("..").locator(".sb-session-role-tooltip");
  await workerRole.focus();
  await expect(workerTooltip).toHaveCSS("opacity", "1");
  await page.locator(".sb-scroll").dispatchEvent("scroll");
  await expect(workerTooltip).toHaveCSS("opacity", "0");

  await workerRole.dispatchEvent("pointerdown", { pointerType: "touch" });
  await workerRole.dispatchEvent("pointerup", { pointerType: "touch" });
  await workerRole.dispatchEvent("click");
  await expect(workerTooltip).toHaveCSS("opacity", "1");
  await page.locator("body").dispatchEvent("pointerdown", { pointerType: "touch" });
  await expect(workerTooltip).toHaveCSS("opacity", "0");

  await workerRole.evaluate((element) => (element as HTMLElement).blur());
  await workerRole.focus();
  await expect(workerTooltip).toHaveCSS("opacity", "1");
  await page.evaluate(() => { window.location.hash = "#/settings/projects"; });
  await expect.poll(() => page.locator(".sb-session-role-tooltip").evaluateAll((tooltips) =>
    tooltips.every((tooltip) => getComputedStyle(tooltip).opacity === "0"),
  )).toBe(true);
});

test("secondary launch and host metadata is absent from compact rows", async ({ page }) => {
  await logIn(page);
  await installFixture(page);

  await expect(page.locator(".sidebar-layout-fixture .sb-launch-folder")).toHaveCount(0);
  await expect(page.locator(".sidebar-layout-fixture .sb-session-meta .worker-chip")).toHaveCount(0);
});
