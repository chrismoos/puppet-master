import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

// The first round an agent produces is the one a reader is most likely
// to be looking at, and it is the round where nothing about the reader's
// position has moved yet. Only a real round can show what the page
// offers there, because the offer depends on a revision the agent
// records by editing the tree.

const FILE = "alpha.txt";

function pm(args: string[]): string {
  return execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
}

function body(edited: boolean, answered: boolean): string {
  return Array.from({ length: 60 }, (_, i) => {
    if (answered && i === 8) return `${FILE} line 9 rewritten by the agent`;
    return edited && i % 4 === 0 ? `${FILE} line ${i + 1} edited` : `${FILE} line ${i + 1}`;
  }).join("\n") + "\n";
}

function seedWorktree(): string {
  const dir = mkdtempSync(join(tmpdir(), "pm-review-first-round-"));
  const git = (...args: string[]) =>
    execFileSync("git", args, {
      cwd: dir,
      encoding: "utf8",
      env: { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" },
    });
  git("init", "-q", "-b", "main");
  git("config", "user.email", "e2e@example.invalid");
  git("config", "user.name", "browser e2e");
  writeFileSync(join(dir, FILE), body(false, false));
  git("add", "-A");
  git("commit", "-q", "-m", "base");
  writeFileSync(join(dir, FILE), body(true, false));
  return dir;
}

function openReview(sessionId: number, worktree: string): number {
  const out = pm(["review", "open", String(sessionId), "--worktree", worktree, "--label", "first round"]);
  const id = out.match(/review (\d+) open/)?.[1];
  if (!id) throw new Error(`could not read a review id from: ${out}`);
  return Number(id);
}

function fileSection(page: Page) {
  return page.locator(`#review-file-${FILE.replace(/[^a-zA-Z0-9_-]/g, "_")}`);
}

async function comment(page: Page, line: string, text: string): Promise<number> {
  await fileSection(page).locator(".review-row", { hasText: line }).first().click();
  const composer = fileSection(page).locator(".review-compose");
  await composer.locator("textarea").fill(text);
  await composer.getByRole("button", { name: "Send", exact: true }).click();
  const thread = fileSection(page).locator(".review-thread", { hasText: text });
  await expect(thread).toBeVisible();
  const id = await thread.getAttribute("id");
  const number = Number(id?.replace("review-thread-", ""));
  if (!Number.isInteger(number)) throw new Error(`could not read a thread id from: ${id}`);
  return number;
}

async function openReviewPage(page: Page, reviewId: number): Promise<void> {
  await logIn(page);
  await page.goto(`${process.env.PM_E2E_BASE_URL}/#/review/${reviewId}`);
  await expect(page.locator(".review-rail-file").first()).toBeVisible();
}

test("the first round the agent produces offers the reader the way onto it", async ({
  page,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const worktree = seedWorktree();
  const reviewId = openReview(session!.id, worktree);
  await openReviewPage(page, reviewId);

  const threadId = await comment(page, `${FILE} line 9 edited`, "why did this move?");

  // The agent's first round: it edits the tree and answers, which is
  // what records Rev 1 as something the reader has not been shown.
  writeFileSync(join(worktree, FILE), body(true, true));
  await isolatedDaemon.agentReply("browser-e2e", threadId, "rewrote it");

  const thread = page.locator(".review-thread", { hasText: "rewrote it" });
  await expect(thread).toBeVisible();

  // A reader who has never advanced has not seen Rev 1, so the round
  // that produced it is something to move onto rather than something
  // they are already reading.
  await expect(thread.getByRole("button", { name: /click to update diff/ })).toBeVisible();
  await expect(thread.locator(".review-thread-ahead.is-current")).toHaveCount(0);

  // The head says the same thing, and does not claim the reader is
  // already reading the revision it is offering them.
  await expect(page.locator(".review-pending")).toContainText("Rev 1 ready");
  await expect(page.locator(".review-reading")).toHaveText("reading Rev 1 as sent");

  // Taking the offer settles it: the reader is on Rev 1, so the control
  // that would pin what is already pinned is gone and the fact remains.
  await thread.getByRole("button", { name: /click to update diff/ }).click();
  await expect(page.locator(".review-pending")).toHaveCount(0);
  await expect(page.locator(".review-reading")).toHaveText("reading Rev 1");
  await expect(thread.locator(".review-thread-ahead.is-current")).toBeVisible();
});
