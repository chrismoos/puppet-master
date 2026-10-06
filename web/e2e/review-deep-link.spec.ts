import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

// The reader's position and the URL are one thing in two forms, which
// only a real browser can show: it takes actual history entries for the
// back button to land anywhere.

const FILES = ["alpha.txt", "beta.txt", "gamma.txt"];

function pm(args: string[]): string {
  return execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
}

function seedWorktree(): string {
  const dir = mkdtempSync(join(tmpdir(), "pm-review-link-"));
  const git = (...args: string[]) =>
    execFileSync("git", args, {
      cwd: dir,
      encoding: "utf8",
      env: { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" },
    });
  const body = (name: string, edited: boolean) =>
    Array.from({ length: 80 }, (_, i) =>
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
  const out = pm(["review", "open", String(sessionId), "--worktree", worktree, "--label", "deep link"]);
  const id = out.match(/review (\d+) open/)?.[1];
  if (!id) throw new Error(`could not read a review id from: ${out}`);
  return Number(id);
}

/** The query the address bar carries, which is the position in its
 * written form. */
function positionInUrl(page: Page): string {
  return new URL(page.url()).hash.split("?")[1] ?? "";
}

function railRow(page: Page, file: string) {
  return page.locator(".review-rail-file", { hasText: file });
}

async function open(page: Page, path: string): Promise<void> {
  await logIn(page);
  await page.goto(`${process.env.PM_E2E_BASE_URL}/#${path}`);
  await expect(page.locator(".review-rail-file").first()).toBeVisible();
}

test("opening a file writes it to the URL and the back button returns to the last one", async ({
  page,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const reviewId = openReview(session!.id, seedWorktree());

  await open(page, `/review/${reviewId}`);
  // A reader who has not moved is at the live tree, which is the
  // default and so says nothing in the URL.
  expect(positionInUrl(page)).toBe("");

  await railRow(page, "alpha.txt").click();
  await expect(page).toHaveURL(/file=alpha\.txt/);

  await railRow(page, "gamma.txt").click();
  await expect(page).toHaveURL(/file=gamma\.txt/);

  await page.goBack();
  await expect(page).toHaveURL(/file=alpha\.txt/);
  await expect(page.locator("#review-file-alpha_txt .review-rows")).toBeVisible();

  await page.goForward();
  await expect(page).toHaveURL(/file=gamma\.txt/);
});

test("a link naming a revision opens on it and says so until the reader comes back", async ({
  page,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const reviewId = openReview(session!.id, seedWorktree());

  // Opening a review records its first snapshot, so there is a revision
  // to name without an agent having answered anything yet.
  await open(page, `/review/${reviewId}?view=sent%3A1`);

  const notice = page.locator(".review-viewing");
  await expect(notice).toContainText("You are viewing");
  await expect(page.locator(".review-select select").first()).toHaveValue("sent:1");

  await notice.getByRole("button", { name: "Back to the working tree" }).click();
  await expect(notice).toHaveCount(0);
  expect(positionInUrl(page)).toBe("");

  // The way back is a history entry, so the reader can undo it.
  await page.goBack();
  await expect(page.locator(".review-viewing")).toContainText("You are viewing");
});

test("a reader who has moved keeps their position when they return to a bare link", async ({
  page,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const reviewId = openReview(session!.id, seedWorktree());

  await open(page, `/review/${reviewId}?view=sent%3A1`);
  await expect(page.locator(".review-viewing")).toBeVisible();

  // The URL wrote through to the stored position, so arriving with no
  // params reconciles the other way and fills the URL back in.
  await open(page, `/review/${reviewId}`);
  await expect(page.locator(".review-viewing")).toBeVisible();
  await expect(page).toHaveURL(/view=sent%3A1/);
});
