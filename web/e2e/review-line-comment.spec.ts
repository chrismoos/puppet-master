import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

// The composer and the open reply belong to one file at a time while the
// file sections around them hold their own state, so what a comment lands
// on, and which file reacts to it, is the contract these cover.

const FILES = ["alpha.txt", "beta.txt"];
const LINES = 40;

function pm(args: string[]): string {
  return execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
}

function seedWorktree(): string {
  const dir = mkdtempSync(join(tmpdir(), "pm-line-comment-"));
  const git = (...args: string[]) =>
    execFileSync("git", args, {
      cwd: dir,
      encoding: "utf8",
      env: { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" },
    });
  const body = (name: string, edited: boolean) =>
    Array.from({ length: LINES }, (_, i) =>
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
  const out = pm(["review", "open", String(sessionId), "--worktree", worktree, "--label", "line comment"]);
  const id = out.match(/review (\d+) open/)?.[1];
  if (!id) throw new Error(`could not read a review id from: ${out}`);
  return Number(id);
}

function fileSection(page: Page, file: string) {
  return page.locator(`#review-file-${file.replace(/[^a-zA-Z0-9_-]/g, "_")}`);
}

/** An edited line, which is one a reader would actually stop on. */
function editedRow(page: Page, file: string, text: string) {
  return fileSection(page, file).locator(".review-row", { hasText: text });
}

async function openReviewPage(page: Page, reviewId: number): Promise<void> {
  await logIn(page);
  await page.goto(`${process.env.PM_E2E_BASE_URL}/#/review/${reviewId}`);
  await expect(page.locator(".review-rail-file").first()).toBeVisible();
}

test("a comment on a diff line opens under it and is saved as a draft", async ({
  page,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const reviewId = openReview(session!.id, seedWorktree());
  await openReviewPage(page, reviewId);

  const alpha = fileSection(page, "alpha.txt");
  await editedRow(page, "alpha.txt", "alpha.txt line 5 edited").first().click();

  const composer = alpha.locator(".review-compose");
  await expect(composer).toBeVisible();
  // The composer opens on the file that was clicked and nowhere else.
  await expect(fileSection(page, "beta.txt").locator(".review-compose")).toHaveCount(0);

  await composer.locator("textarea").fill("this rename reads better than the old one");
  await composer.getByRole("button", { name: "Save as draft" }).click();

  const thread = alpha.locator(".review-thread", {
    hasText: "this rename reads better than the old one",
  });
  await expect(thread).toBeVisible();
  await expect(thread).toHaveClass(/is-draft/);
  await expect(composer).toHaveCount(0);
  // A draft is the review's, so the page's own count picks it up.
  await expect(page.locator(".badge", { hasText: "1 draft" })).toBeVisible();

  // The thread is anchored to the line it was written on, so it survives
  // the reader leaving the page and coming back.
  await page.reload();
  await expect(
    fileSection(page, "alpha.txt").locator(".review-thread", {
      hasText: "this rename reads better than the old one",
    }),
  ).toBeVisible();
});

test("typing a comment leaves the other file's lines alone", async ({
  page,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const reviewId = openReview(session!.id, seedWorktree());
  await openReviewPage(page, reviewId);

  const beta = fileSection(page, "beta.txt");
  const betaRow = editedRow(page, "beta.txt", "beta.txt line 9 edited").first();
  await expect(betaRow).toBeVisible();

  // The rows of a file nobody is typing in must survive untouched, which
  // is what lets them stay out of the render.
  const betaRowsBefore = await beta.locator(".review-row").count();
  await editedRow(page, "alpha.txt", "alpha.txt line 5 edited").first().click();
  await fileSection(page, "alpha.txt")
    .locator(".review-compose textarea")
    .fill("a longer thought, typed out");

  await expect(beta.locator(".review-row")).toHaveCount(betaRowsBefore);
  await expect(betaRow).toHaveText(/beta\.txt line 9 edited/);
  await expect(beta.locator(".review-compose")).toHaveCount(0);
});

test("a comment can be replied to and resolved without disturbing its file", async ({
  page,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const reviewId = openReview(session!.id, seedWorktree());
  await openReviewPage(page, reviewId);

  const alpha = fileSection(page, "alpha.txt");
  await editedRow(page, "alpha.txt", "alpha.txt line 9 edited").first().click();
  const composer = alpha.locator(".review-compose");
  await composer.locator("textarea").fill("why did this move?");
  await composer.getByRole("button", { name: "Send", exact: true }).click();

  const thread = alpha.locator(".review-thread", { hasText: "why did this move?" });
  await expect(thread).toBeVisible();

  await thread.getByRole("button", { name: "Reply" }).click();
  // The reply box belongs to the thread that opened it, so no other
  // thread on the page grows one.
  await expect(page.locator(".review-reply")).toHaveCount(1);
  await thread.locator(".review-reply").fill("it followed the caller");
  await thread.getByRole("button", { name: "Send reply" }).click();

  await expect(
    alpha.locator(".review-thread", { hasText: "it followed the caller" }),
  ).toBeVisible();
  await expect(page.locator(".review-reply")).toHaveCount(0);

  const answered = alpha.locator(".review-thread", { hasText: "it followed the caller" });
  await answered.getByRole("button", { name: "Resolve" }).click();
  // A resolved thread collapses to its first line and stays reachable.
  await expect(alpha.locator(".review-thread.is-resolved.is-collapsed")).toBeVisible();
  await alpha.locator(".review-thread-reopen").first().click();
  await expect(alpha.locator(".review-thread", { hasText: "it followed the caller" })).toBeVisible();
});
