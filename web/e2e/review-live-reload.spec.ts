import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Locator, type Page } from "./fixtures";
import { logIn } from "./support";

// A reader on the working tree is moved onto each new round as it lands.
// That move must never happen under a comment being written, and the
// part of the page the reader is looking at must stay where it was.

const ABOVE = "alpha.txt";
const BELOW = "beta.txt";
// Room under the reader, so holding their place is never cut short by
// the end of the page.
const AFTER = "gamma.txt";
const LINES = 60;
const INSERTED = 12;

// One tree comparison interval plus the diff refetch that follows it.
const NOTICE_MS = 20_000;
// Long enough for several tree checks to have come and gone.
const SETTLE_MS = 8_000;
// Subpixel rounding is all a row that stayed put is allowed to move.
const STILL_PX = 2;
const LAYOUT_SETTLE_MS = 250;
const WHEEL_PX = 120;
const WHEEL_STEPS = 40;
const WHEEL_STOP_Y = 500;

function pm(args: string[]): string {
  return execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
}

function body(file: string, mark: string, extra = 0): string {
  const head = Array.from({ length: extra }, (_, i) => `${file} inserted ${i + 1}`);
  const rest = Array.from({ length: LINES }, (_, i) =>
    i % 3 === 0 ? `${file} line ${i + 1} ${mark}` : `${file} line ${i + 1}`,
  );
  return [...head, ...rest].join("\n") + "\n";
}

function seedWorktree(): string {
  const dir = mkdtempSync(join(tmpdir(), "pm-review-live-"));
  const git = (...args: string[]) =>
    execFileSync("git", args, {
      cwd: dir,
      encoding: "utf8",
      env: { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" },
    });
  git("init", "-q", "-b", "main");
  git("config", "user.email", "e2e@example.invalid");
  git("config", "user.name", "browser e2e");
  const files = [ABOVE, BELOW, AFTER];
  for (const file of files) writeFileSync(join(dir, file), body(file, "base"));
  git("add", "-A");
  git("commit", "-q", "-m", "base");
  for (const file of files) writeFileSync(join(dir, file), body(file, "edited"));
  return dir;
}

/** The agent's next round grows the file above, which is what shifts
 * everything under it. */
function growAbove(worktree: string): void {
  writeFileSync(join(worktree, ABOVE), body(ABOVE, "edited", INSERTED));
}

function openReview(sessionId: number, worktree: string): number {
  const out = pm(["review", "open", String(sessionId), "--worktree", worktree, "--label", "live reload"]);
  const id = out.match(/review (\d+) open/)?.[1];
  if (!id) throw new Error(`could not read a review id from: ${out}`);
  return Number(id);
}

function fileSection(page: Page, file: string): Locator {
  return page.locator(`#review-file-${file.replace(/[^a-zA-Z0-9_-]/g, "_")}`);
}

function row(page: Page, file: string, text: string): Locator {
  return fileSection(page, file).locator(".review-row", { hasText: text }).first();
}

async function openReviewPage(page: Page, reviewId: number): Promise<void> {
  await logIn(page);
  await page.goto(`${process.env.PM_E2E_BASE_URL}/#/review/${reviewId}`);
  await expect(fileSection(page, BELOW).locator(".review-rows")).toContainText(`${BELOW} line 1 edited`);
}

async function sentThread(page: Page, file: string, line: string, text: string): Promise<number> {
  await row(page, file, line).click();
  const composer = fileSection(page, file).locator(".review-compose");
  await composer.locator("textarea").fill(text);
  await composer.getByRole("button", { name: "Send", exact: true }).click();
  const thread = fileSection(page, file).locator(".review-thread", { hasText: text });
  await expect(thread).toBeVisible();
  return Number((await thread.getAttribute("id"))?.replace("review-thread-", ""));
}

async function top(locator: Locator): Promise<number> {
  const box = await locator.boundingBox();
  if (!box) throw new Error("not laid out");
  return box.y;
}

/** Where a row sits once the files around it have been laid out, which
 * happens a frame or more after a scroll brings them near. */
async function settledTop(locator: Locator): Promise<number> {
  let last = await top(locator);
  await expect(async () => {
    const now = await top(locator);
    const still = Math.abs(now - last) <= STILL_PX;
    last = now;
    expect(still).toBe(true);
  }).toPass({ intervals: [LAYOUT_SETTLE_MS] });
  return last;
}

/** The row the open composer sits under, read from the page rather than
 * from the composer's own state. */
async function rowAboveComposer(page: Page, file: string): Promise<string> {
  return fileSection(page, file).locator(".review-compose").evaluate((composer) => {
    const rows = [...composer.closest(".review-rows")!.querySelectorAll(".review-row")];
    const above = rows.filter(
      (row) => row.compareDocumentPosition(composer) & Node.DOCUMENT_POSITION_FOLLOWING,
    );
    return above.at(-1)?.textContent ?? "";
  });
}

test("a new round waits while a comment is being written further down", async ({ page, isolatedDaemon }) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const worktree = seedWorktree();
  await openReviewPage(page, openReview(session!.id, worktree));
  const threadId = await sentThread(page, ABOVE, `${ABOVE} line 4 edited`, "tidy this");

  // Opened straight after a send, before the send has come back, which
  // is how a reader working down a file actually moves.
  const target = `${BELOW} line 32`;
  await row(page, BELOW, target).scrollIntoViewIfNeeded();
  await row(page, BELOW, target).click();
  const composer = fileSection(page, BELOW).locator(".review-compose");
  const textarea = composer.locator("textarea");
  await expect(textarea).toBeFocused();
  const before = await settledTop(composer);

  growAbove(worktree);
  await isolatedDaemon.agentReply("browser-e2e", threadId, "tidied");
  await expect(page.locator(".review-thread", { hasText: "tidied" })).toBeVisible();
  await expect(page.locator(".review-pending")).toBeVisible({ timeout: NOTICE_MS });
  // e2e-real-time-wait: the contract is that nothing replaces the diff over several tree checks.
  await page.waitForTimeout(SETTLE_MS);

  // Nothing typed yet, but the box is open: the reader is mid-thought.
  await expect(fileSection(page, ABOVE).locator(".review-rows")).not.toContainText(`${ABOVE} inserted 1`);
  await expect(textarea).toBeFocused();
  expect(await rowAboveComposer(page, BELOW)).toContain(target);
  expect(Math.abs((await top(composer)) - before)).toBeLessThanOrEqual(STILL_PX);

  // Closing the box is what lets the round in.
  await composer.getByRole("button", { name: "Cancel" }).click();
  await expect(fileSection(page, ABOVE).locator(".review-rows")).toContainText(`${ABOVE} inserted 1`, {
    timeout: NOTICE_MS,
  });
  await expect(page.locator(".review-pending")).toHaveCount(0);
  await expect(page.locator(".review-stale")).toHaveCount(0);
});

test("an open composer stays on its line when its own file grows above it", async ({ page, isolatedDaemon }) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const worktree = seedWorktree();
  await openReviewPage(page, openReview(session!.id, worktree));

  const target = `${ABOVE} line 32`;
  await row(page, ABOVE, target).scrollIntoViewIfNeeded();
  await row(page, ABOVE, target).click();
  const composer = fileSection(page, ABOVE).locator(".review-compose");
  await expect(composer.locator("textarea")).toBeFocused();

  growAbove(worktree);
  await expect(page.locator(".review-stale")).toBeVisible({ timeout: NOTICE_MS });

  await expect(fileSection(page, ABOVE).locator(".review-rows")).not.toContainText(`${ABOVE} inserted 1`);
  expect(await rowAboveComposer(page, ABOVE)).toContain(target);
});

test("a round that lands while reading keeps the reader's place", async ({ page, isolatedDaemon }) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const worktree = seedWorktree();
  await openReviewPage(page, openReview(session!.id, worktree));
  const threadId = await sentThread(page, ABOVE, `${ABOVE} line 4 edited`, "tidy this");

  const anchor = row(page, BELOW, `${BELOW} line 40 edited`);
  // Scrolled there the way a reader gets there, so the files passed on
  // the way have been laid out at their real height.
  await page.locator(".review-diff").hover();
  for (let i = 0; i < WHEEL_STEPS && ((await anchor.boundingBox())?.y ?? Infinity) > WHEEL_STOP_Y; i++) {
    await page.mouse.wheel(0, WHEEL_PX);
    await page.evaluate(() => new Promise((done) => requestAnimationFrame(() => requestAnimationFrame(done))));
  }
  const before = await settledTop(anchor);

  growAbove(worktree);
  await isolatedDaemon.agentReply("browser-e2e", threadId, "tidied");
  await expect(fileSection(page, ABOVE).locator(".review-rows")).toContainText(`${ABOVE} inserted 1`, {
    timeout: NOTICE_MS,
  });
  await expect(page.locator(".review-pending")).toHaveCount(0);
  await expect(page.locator(".review-stale")).toHaveCount(0);
  expect(Math.abs((await top(anchor)) - before)).toBeLessThanOrEqual(STILL_PX);
});
