import { expect, test } from "./fixtures";
import { logIn } from "./support";

const LONG_ACTIVITY = "Reviewing the focused browser contract and updating the Agent INFO layout so the current work remains readable across normal desktop and narrow widths.";
const LONG_TOKEN = "activity-token-without-natural-breaks-0123456789ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz";
const SHORT_ACTIVITY = "Running tests";
const CHECKPOINT_TEXT = `Checkpoint headline — ${LONG_ACTIVITY} ${LONG_TOKEN}`;
const STATUS_TEXT = "Status task: implementing — full-width report row";
const PROGRESS_TEXT = "73% verifying browser behavior";
const BLOCKED_TEXT = "Which accessible dropdown behavior should remain stable?";

async function openInfo(page: import("@playwright/test").Page, title: string) {
  await page.locator(".sb-session-row").filter({
    has: page.locator(".sb-session-title", { hasText: new RegExp(`^${title}$`) }),
  }).locator(".sb-session").click();
  const toggle = page.getByRole("button", { name: "info", exact: true });
  if (await toggle.getAttribute("aria-pressed") !== "true") await toggle.click();
  return page.getByRole("complementary", { name: "session context" });
}

async function assertContained(panel: import("@playwright/test").Locator): Promise<void> {
  const panelMetrics = await panel.evaluate((element) => ({
    clientWidth: element.clientWidth,
    scrollWidth: element.scrollWidth,
  }));
  expect(panelMetrics.scrollWidth).toBeLessThanOrEqual(panelMetrics.clientWidth);

  const metrics = await panel.locator(".is-activity").evaluate((element) => {
    const cell = element.querySelector<HTMLElement>(".ctx-grid-cell")!;
    const style = getComputedStyle(cell);
    return {
      rowWidth: element.getBoundingClientRect().width,
      cellWidth: cell.getBoundingClientRect().width,
      clientWidth: cell.clientWidth,
      scrollWidth: cell.scrollWidth,
      whiteSpace: style.whiteSpace,
      overflowWrap: style.overflowWrap,
    };
  });
  expect(metrics.cellWidth).toBeGreaterThan(metrics.rowWidth * 0.95);
  expect(metrics.scrollWidth).toBeLessThanOrEqual(metrics.clientWidth);
  expect(metrics.whiteSpace).toBe("pre-wrap");
  expect(metrics.overflowWrap).toBe("anywhere");
}

test("persisted session Activity spans the live INFO panel and wraps", async ({ page, isolatedDaemon }) => {
  isolatedDaemon.setSessionActivity("browser-e2e", `${LONG_ACTIVITY}\n${LONG_TOKEN}`);
  isolatedDaemon.setSessionActivity("browser-e2e-two", SHORT_ACTIVITY);
  await logIn(page);

  const shortPanel = await openInfo(page, "browser-e2e-two");
  const shortActivity = shortPanel.locator(".ctx-grid-row.is-activity");
  await expect(shortActivity).toContainText(SHORT_ACTIVITY);
  const shortHeight = (await shortActivity.boundingBox())!.height;

  const panel = await openInfo(page, "browser-e2e");
  const activity = panel.locator(".ctx-grid-row.is-activity");
  await expect(activity).toContainText(LONG_ACTIVITY);
  await expect(activity).toContainText(LONG_TOKEN);
  await assertContained(panel);
  expect((await activity.boundingBox())!.height).toBeGreaterThan(shortHeight);

  await page.setViewportSize({ width: 520, height: 700 });
  await expect(activity).toBeVisible();
  await assertContained(panel);
  expect((await activity.boundingBox())!.height).toBeGreaterThan(80);
});

async function assertTimelineContained(panel: import("@playwright/test").Locator): Promise<void> {
  const metrics = await panel.locator(".timeline-item").evaluateAll((items) => items.map((item) => {
    const message = item.querySelector<HTMLElement>(".timeline-text")!;
    const itemRect = item.getBoundingClientRect();
    const messageRect = message.getBoundingClientRect();
    const style = getComputedStyle(message);
    return {
      itemWidth: itemRect.width,
      messageWidth: messageRect.width,
      itemScrollWidth: item.scrollWidth,
      itemClientWidth: item.clientWidth,
      messageScrollWidth: message.scrollWidth,
      messageClientWidth: message.clientWidth,
      messageTop: messageRect.top,
      metadataBottom: item.querySelector<HTMLElement>(".timeline-meta")!.getBoundingClientRect().bottom,
      overflowWrap: style.overflowWrap,
    };
  }));
  for (const item of metrics) {
    expect(item.messageTop).toBeGreaterThanOrEqual(item.metadataBottom);
    expect(item.messageWidth).toBeGreaterThan(item.itemWidth * 0.95);
    expect(item.messageScrollWidth).toBeLessThanOrEqual(item.messageClientWidth);
    expect(item.itemScrollWidth).toBeLessThanOrEqual(item.itemClientWidth);
    expect(item.overflowWrap).toBe("anywhere");
  }
}

test("INFO Activity reports use full-width wrapping lines and preserve dropdown semantics", async ({ page, isolatedDaemon }) => {
  const now = Date.now();
  isolatedDaemon.setSessionActivity("browser-e2e", `${LONG_ACTIVITY}\n${LONG_TOKEN}`);
  isolatedDaemon.seedSessionReports("browser-e2e", [
    { tsUnixMs: now - 30_000, kind: "progress", payload: { percent: 73, summary: "verifying browser behavior" } },
    { tsUnixMs: now - 10_000, kind: "checkpoint", payload: { headline: "Checkpoint headline", note: `${LONG_ACTIVITY} ${LONG_TOKEN}` } },
    { tsUnixMs: now - 40_000, kind: "blocked", payload: { question: BLOCKED_TEXT } },
    { tsUnixMs: now - 20_000, kind: "status", payload: { task: "Status task", phase: "implementing", detail: "full-width report row" } },
  ]);
  await logIn(page);

  const panel = await openInfo(page, "browser-e2e");
  const toggle = panel.getByRole("button", { name: /activity 4/i });
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  const controlledId = await toggle.getAttribute("aria-controls");
  expect(controlledId).toBeTruthy();
  await toggle.click();
  await expect(toggle).toBeFocused();
  await expect(toggle).toHaveAttribute("aria-expanded", "true");
  await expect(panel.locator(`#${controlledId}`)).toBeVisible();
  await expect(panel.locator(".timeline-time").first()).toHaveAttribute("datetime", /^\d{4}-/);

  const messages = panel.locator(".timeline-text");
  await expect(messages).toHaveText([CHECKPOINT_TEXT, STATUS_TEXT, PROGRESS_TEXT, BLOCKED_TEXT]);
  await assertTimelineContained(panel);

  await page.setViewportSize({ width: 420, height: 700 });
  await expect(messages.first()).toBeVisible();
  await assertTimelineContained(panel);
  expect((await messages.first().boundingBox())!.height).toBeGreaterThan(50);

  await toggle.click();
  await expect(toggle).toBeFocused();
  await expect(toggle).toHaveAttribute("aria-expanded", "false");
  await expect(messages).toHaveCount(0);
});
