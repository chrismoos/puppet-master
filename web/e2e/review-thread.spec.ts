import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

// One anchor holds one conversation, and settling it moves the reader
// on. Both are things only a real page can show: the first is about how
// many threads a line ends up with, the second about where the viewport
// lands.

const FILES = ["alpha.txt", "beta.txt"];

function pm(args: string[]): string {
  return execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
}

function seedWorktree(): string {
  const dir = mkdtempSync(join(tmpdir(), "pm-review-thread-"));
  const git = (...args: string[]) =>
    execFileSync("git", args, {
      cwd: dir,
      encoding: "utf8",
      env: { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" },
    });
  const body = (name: string, edited: boolean) =>
    Array.from({ length: 120 }, (_, i) =>
      edited && i % 4 === 0 ? `${name} line ${i + 1} edited` : `${name} line ${i + 1}`,
    ).join("\n") + "\n";

  git("init", "-q", "-b", "main");
  git("config", "user.email", "e2e@example.invalid");
  git("config", "user.name", "browser e2e");
  for (const name of FILES) writeFileSync(join(dir, name), body(name, false));
  git("add", "-A");
  git("commit", "-q", "-m", "base");
  for (const name of FILES) writeFileSync(join(dir, name), body(name, true));
  return dir;
}

function openReview(sessionId: number, worktree: string): number {
  const out = pm(["review", "open", String(sessionId), "--worktree", worktree, "--label", "threads"]);
  const id = out.match(/review (\d+) open/)?.[1];
  if (!id) throw new Error(`could not read a review id from: ${out}`);
  return Number(id);
}

function fileSection(page: Page, file: string) {
  return page.locator(`#review-file-${file.replace(/[^a-zA-Z0-9_-]/g, "_")}`);
}

function editedRow(page: Page, file: string, text: string) {
  return fileSection(page, file).locator(".review-row", { hasText: text });
}

async function comment(page: Page, file: string, line: string, body: string): Promise<void> {
  await editedRow(page, file, line).first().click();
  const composer = fileSection(page, file).locator(".review-compose");
  await composer.locator("textarea").fill(body);
  await composer.getByRole("button", { name: "Send", exact: true }).click();
  await expect(fileSection(page, file).locator(".review-thread", { hasText: body })).toBeVisible();
}

/** Whether a thread sits inside the scrolling diff rather than off it,
 * which is what landing on it means to a reader. */
async function inView(page: Page, text: string): Promise<boolean> {
  return page
    .locator(".review-thread", { hasText: text })
    .first()
    .evaluate((node) => {
      const diff = document.querySelector(".review-diff");
      if (!diff) return false;
      const a = node.getBoundingClientRect();
      const b = diff.getBoundingClientRect();
      return a.bottom > b.top && a.top < b.bottom;
    });
}

async function openReviewPage(page: Page, reviewId: number): Promise<void> {
  await logIn(page);
  await page.goto(`${process.env.PM_E2E_BASE_URL}/#/review/${reviewId}`);
  await expect(page.locator(".review-rail-file").first()).toBeVisible();
}

test("a reply joins the thread it answers rather than starting another one", async ({
  page,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const reviewId = openReview(session!.id, seedWorktree());
  await openReviewPage(page, reviewId);

  const alpha = fileSection(page, "alpha.txt");
  await comment(page, "alpha.txt", "alpha.txt line 9 edited", "why did this move?");
  await expect(alpha.locator(".review-thread")).toHaveCount(1);

  const thread = alpha.locator(".review-thread", { hasText: "why did this move?" });
  await thread.getByRole("button", { name: "Reply" }).click();
  await thread.locator(".review-reply").fill("and what called it before?");
  await thread.getByRole("button", { name: "Send reply" }).click();

  // The conversation stays one thread, holding both turns in order.
  await expect(alpha.locator(".review-thread")).toHaveCount(1);
  await expect(alpha.locator(".review-thread")).toContainText("why did this move?");
  await expect(alpha.locator(".review-thread")).toContainText("and what called it before?");
});

test("resolving a thread carries the reader to the next open one", async ({
  page,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const reviewId = openReview(session!.id, seedWorktree());
  await openReviewPage(page, reviewId);

  await comment(page, "alpha.txt", "alpha.txt line 9 edited", "first question");
  await comment(page, "beta.txt", "beta.txt line 101 edited", "second question");

  // Reading the first one puts the far one off screen, which is the
  // position a reader is actually in when they settle something.
  await editedRow(page, "alpha.txt", "alpha.txt line 9 edited").first().scrollIntoViewIfNeeded();
  expect(await inView(page, "second question")).toBe(false);

  const first = page.locator(".review-thread", { hasText: "first question" });
  await first.getByRole("button", { name: "Resolve" }).click();

  await expect
    .poll(() => inView(page, "second question"))
    .toBe(true);
});
