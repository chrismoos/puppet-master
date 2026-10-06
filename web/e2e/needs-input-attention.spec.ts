import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

function sessionRow(page: Page, title: string) {
  return page.locator(".sb-session-title")
    .filter({ hasText: new RegExp(`^${title}$`) })
    .locator("xpath=ancestor::div[contains(@class, 'sb-session-row')][1]");
}

async function openSession(page: Page, title: string): Promise<string> {
  await sessionRow(page, title).locator(".sb-session").click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);
  return new URL(page.url()).hash.match(/\/session\/(\d+)$/)![1];
}

test("NeedsInput stays blocked until submission and persists seen descendant attention", async ({ page, isolatedDaemon }) => {
  await logIn(page);
  const first = "browser-e2e";
  const second = "browser-e2e-two";
  await openSession(page, first);

  // Enable real xterm mouse tracking before the session blocks, then trigger
  // NeedsInput from the retained terminal while another session is selected.
  const terminal = page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
  await terminal.focus();
  await page.keyboard.type("mouseon");
  await page.keyboard.press("Enter");
  await openSession(page, second);
  isolatedDaemon.hookSession(first, "needs-input", "choose a path");

  const firstRow = sessionRow(page, first);
  await expect(firstRow).toHaveClass(/is-attention-unseen/);
  await expect(firstRow.locator(".sb-session")).toHaveAttribute("aria-label", /unseen attention/);
  await expect(firstRow).toHaveCSS("animation-name", "pulse-amber");

  // Viewing reduces urgency but does not acknowledge lifecycle, and the read
  // state is durable across a full browser refresh.
  await firstRow.locator(".sb-session").click();
  await expect(firstRow).toHaveClass(/is-attention-seen/);
  await expect(firstRow.locator(".sb-session")).toHaveAttribute("aria-label", /viewed but unanswered/);
  await expect(firstRow).toHaveCSS("animation-name", "none");
  await page.reload();
  await expect(sessionRow(page, first)).toHaveClass(/is-attention-seen/);

  // Partial typing and a real mouse-tracking click produce terminal bytes but
  // cannot clear NeedsInput. Only the semantic Enter submission may do so.
  const selectedTerminal = page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
  await selectedTerminal.focus();
  await page.keyboard.type("n");
  const screen = await page.locator('.term-layer[style*="visible"] .xterm-screen').boundingBox();
  if (!screen) throw new Error("visible terminal screen missing");
  await page.mouse.click(screen.x + screen.width / 2, screen.y + screen.height / 2);
  await expect(sessionRow(page, first)).toHaveClass(/is-needs-input/);
  await page.keyboard.press("Enter");
  await expect(sessionRow(page, first)).not.toHaveClass(/is-needs-input/);

  // A later transition resets attention to unseen. A collapsed bucket header
  // preserves that distinction, then switches to restrained seen.
  await openSession(page, second);
  isolatedDaemon.hookSession(first, "needs-input", "choose again");
  await expect(sessionRow(page, first)).toHaveClass(/is-attention-unseen/);

  const bucketHead = page.locator(".sb-bucket-head").filter({ hasText: "browser-e2e" });
  const bucketIndicator = bucketHead.locator(".sb-descendant-attention");
  await bucketHead.click();
  await expect(bucketIndicator).toHaveClass(/is-unseen/);
  await expect(bucketIndicator).toHaveAttribute("aria-label", /1 unseen/);

  await bucketHead.click();
  await sessionRow(page, first).locator(".sb-session").click();
  await expect(sessionRow(page, first)).toHaveClass(/is-attention-seen/);

  await bucketHead.click();
  await expect(bucketIndicator).toHaveClass(/is-seen/);
  await expect(bucketIndicator).toHaveAttribute("aria-label", /still need input/);
});
