import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

// A review reads a working tree, and that tree keeps moving while the
// reader is on the page. Every existing way the page learns something
// changed goes through an agent reply, so a plain edit — the shape an
// agent working alongside the reader actually produces — is the case
// with no path to the reader at all.

const TRACKED = "alpha.txt";
const ADDED = "beta.txt";
const LINES = 60;

// How long the page is given to notice the tree moved. It has to cover
// one comparison interval plus the diff refetch that follows it.
const NOTICE_MS = 20_000;

function pm(args: string[]): string {
  return execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
}

function body(mark: string | null): string {
  return Array.from({ length: LINES }, (_, i) =>
    mark && i % 4 === 0 ? `${TRACKED} line ${i + 1} ${mark}` : `${TRACKED} line ${i + 1}`,
  ).join("\n") + "\n";
}

/** A committed base with an uncommitted edit, which is what a review of
 * work in progress reads. */
function seedWorktree(): string {
  const dir = mkdtempSync(join(tmpdir(), "pm-review-stale-"));
  const git = (...args: string[]) =>
    execFileSync("git", args, {
      cwd: dir,
      encoding: "utf8",
      env: { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" },
    });
  git("init", "-q", "-b", "main");
  git("config", "user.email", "e2e@example.invalid");
  git("config", "user.name", "browser e2e");
  writeFileSync(join(dir, TRACKED), body(null));
  git("add", "-A");
  git("commit", "-q", "-m", "base");
  writeFileSync(join(dir, TRACKED), body("edited"));
  return dir;
}

function openReview(sessionId: number, worktree: string): number {
  const out = pm([
    "review", "open", String(sessionId), "--worktree", worktree, "--label", "worktree staleness",
  ]);
  const id = out.match(/review (\d+) open/)?.[1];
  if (!id) throw new Error(`could not read a review id from: ${out}`);
  return Number(id);
}

function fileSection(page: Page, file: string) {
  return page.locator(`#review-file-${file.replace(/[^a-zA-Z0-9_-]/g, "_")}`);
}

function railFile(page: Page, file: string) {
  return page.locator(`[data-rail-file="${file}"]`);
}

async function openReviewPage(page: Page, reviewId: number): Promise<void> {
  await logIn(page);
  await page.goto(`${process.env.PM_E2E_BASE_URL}/#/review/${reviewId}`);
  await expect(page.locator(".review-rail-file").first()).toBeVisible();
}

/** Leaves a comment and returns its thread id, which an agent reply needs. */
async function comment(page: Page, line: string, text: string): Promise<number> {
  const section = fileSection(page, TRACKED);
  await section.locator(".review-row", { hasText: line }).first().click();
  const composer = section.locator(".review-compose");
  await composer.locator("textarea").fill(text);
  await composer.getByRole("button", { name: "Send", exact: true }).click();
  const thread = section.locator(".review-thread", { hasText: text });
  await expect(thread).toBeVisible();
  const id = await thread.getAttribute("id");
  const number = Number(id?.replace("review-thread-", ""));
  if (!Number.isInteger(number)) throw new Error(`could not read a thread id from: ${id}`);
  return number;
}

/** The tree moves with no agent reply and no new revision. */
function moveWorktree(worktree: string): void {
  writeFileSync(join(worktree, TRACKED), body("edited again"));
  writeFileSync(join(worktree, ADDED), `${ADDED} arrived after the page loaded\n`);
}

test("a worktree that moves under a live reader reaches the page", async ({
  page,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const worktree = seedWorktree();
  await openReviewPage(page, openReview(session!.id, worktree));

  const rows = fileSection(page, TRACKED).locator(".review-rows");
  await expect(rows).toContainText(`${TRACKED} line 1 edited`);
  await expect(railFile(page, ADDED)).toHaveCount(0);

  moveWorktree(worktree);

  // Nothing here is pinned and nothing is half-written, so the reader is
  // taken to what the tree now says rather than left on a stale render.
  await expect(rows).toContainText(`${TRACKED} line 1 edited again`, { timeout: NOTICE_MS });
  await expect(railFile(page, ADDED)).toHaveCount(1);
  await expect(page.locator(".review-stale")).toHaveCount(0);
});

test("a half-written comment is offered the newer tree rather than losing it", async ({
  page,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const worktree = seedWorktree();
  await openReviewPage(page, openReview(session!.id, worktree));

  const section = fileSection(page, TRACKED);
  const rows = section.locator(".review-rows");
  await expect(rows).toContainText(`${TRACKED} line 1 edited`);

  // An unsent draft on the file that is about to change. Replacing the
  // diff under it would take the line the comment is being written
  // against out from under the composer.
  await section.locator(".review-row", { hasText: `${TRACKED} line 5 edited` }).first().click();
  const composer = section.locator(".review-compose");
  await composer.locator("textarea").fill("still thinking about this one");

  moveWorktree(worktree);

  const stale = page.locator(".review-stale");
  await expect(stale).toBeVisible({ timeout: NOTICE_MS });
  await expect(stale).toContainText(TRACKED);
  await expect(stale).toContainText(ADDED);

  // Offered, not taken: the render and the draft both survive being told.
  await expect(rows).not.toContainText(`${TRACKED} line 1 edited again`);
  await expect(composer.locator("textarea")).toHaveValue("still thinking about this one");

  await stale.getByRole("button", { name: "Refresh" }).click();
  await expect(rows).toContainText(`${TRACKED} line 1 edited again`);
  await expect(railFile(page, ADDED)).toHaveCount(1);
  await expect(stale).toHaveCount(0);
});

test("a reply after the tree moved still records its round, and a stored view keeps its content", async ({
  page,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const worktree = seedWorktree();
  await openReviewPage(page, openReview(session!.id, worktree));

  const rows = fileSection(page, TRACKED).locator(".review-rows");
  await expect(rows).toContainText(`${TRACKED} line 1 edited`);
  const threadId = await comment(page, `${TRACKED} line 9 edited`, "why did this move?");

  moveWorktree(worktree);
  await expect(rows).toContainText(`${TRACKED} line 1 edited again`, { timeout: NOTICE_MS });

  // The reply records the tree the agent left as the round's revision,
  // which is what the picker offers.
  await isolatedDaemon.agentReply("browser-e2e", threadId, "rewrote it");
  await expect(page.locator(".review-thread", { hasText: "rewrote it" })).toBeVisible();
  const picker = page.getByRole("combobox", { name: "view" });
  await expect(picker.locator("option[value='sent:1']")).toHaveCount(1);

  // What the agent was sent is a stored revision, so it reads as it did
  // when it was sent no matter where the tree has gone since.
  await picker.selectOption("sent:1");
  await expect(rows).toContainText(`${TRACKED} line 1 edited`);
  await expect(rows).not.toContainText(`${TRACKED} line 1 edited again`);

  writeFileSync(join(worktree, TRACKED), body("edited a third time"));

  // Told, never replaced: a reader who deliberately left the working
  // tree keeps the revision they chose.
  const stale = page.locator(".review-stale");
  await expect(stale).toBeVisible({ timeout: NOTICE_MS });
  await expect(rows).toContainText(`${TRACKED} line 1 edited`);
  await expect(rows).not.toContainText(`${TRACKED} line 1 edited a third time`);
});
