import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Locator, type Page } from "./fixtures";
import { logIn } from "./support";

const FILES = ["alpha.txt", "beta.txt", "gamma.txt", "delta.txt"];
const LINES = 120;

function pm(args: string[]): string {
  return execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
}

/** A committed base with working-tree edits, which is what a review of
 * uncommitted work reads. The files are long and edited throughout, so
 * each one fills more than a screen the way a real review does. */
function seedWorktree(): string {
  const dir = mkdtempSync(join(tmpdir(), "pm-review-"));
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
  const out = pm(["review", "open", String(sessionId), "--worktree", worktree, "--label", "file list"]);
  const id = out.match(/review (\d+) open/)?.[1];
  if (!id) throw new Error(`could not read a review id from: ${out}`);
  return Number(id);
}

/** How far a file's header sits from the top of the scrolling diff, which
 * is what "at the top of the screen" means here. */
async function headerOffset(page: Page, file: string): Promise<number> {
  const id = `review-file-${file.replace(/[^a-zA-Z0-9_-]/g, "_")}`;
  return page.evaluate((fileId) => {
    const head = document.querySelector(`#${fileId} .review-file-head`);
    const diff = document.querySelector(".review-diff");
    if (!head || !diff) return Number.NaN;
    return head.getBoundingClientRect().top - diff.getBoundingClientRect().top;
  }, id);
}

function viewportHeight(page: Page): Promise<number> {
  return page.locator(".review-diff").evaluate((el) => el.clientHeight);
}

/** The reader is as far down the review as it goes. */
function atDocumentEnd(page: Page): Promise<boolean> {
  return page.locator(".review-diff").evaluate(
    (el) => Math.abs(el.scrollHeight - el.clientHeight - el.scrollTop) <= 2,
  );
}

function railWidth(page: Page): Promise<number> {
  return page.locator(".review-rail").evaluate((el) => el.getBoundingClientRect().width);
}

/** What a set of circles counts between them. */
async function total(counts: Locator): Promise<number> {
  const texts = await counts.allInnerTexts();
  return texts.reduce((sum, text) => sum + Number(text), 0);
}

function row(page: Page, file: string) {
  return page.locator(".review-rail-file", { hasText: file });
}

async function openReviewPage(page: Page, reviewId: number): Promise<void> {
  await logIn(page);
  await page.goto(`${process.env.PM_E2E_BASE_URL}/#/review/${reviewId}`);
  await expect(page.locator(".review-rail-file").first()).toBeVisible();
}

test("a viewed file reopens from the file list, which stays resizable across reloads", async ({
  page,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const reviewId = openReview(session!.id, seedWorktree());

  await openReviewPage(page, reviewId);

  const alphaSection = page.locator("#review-file-alpha_txt");
  const alphaRows = alphaSection.locator(".review-rows");
  const alphaViewed = alphaSection.getByRole("checkbox", { name: "Viewed" });
  const alphaRailRow = row(page, "alpha.txt");
  await expect(alphaRows).toBeVisible();

  // Marking it viewed collapses the file to its header.
  await alphaViewed.click();
  await expect(alphaRows).toHaveCount(0);
  await expect(alphaRailRow).toHaveClass(/is-viewed/);

  // Clicking the row is the reader asking to see it again, so it comes
  // back rather than scrolling to a collapsed header.
  await alphaRailRow.click();
  await expect(alphaRows).toBeVisible();
  await expect(alphaViewed).not.toBeChecked();
  await expect(alphaRailRow).not.toHaveClass(/is-viewed/);

  // The other file is untouched by that click.
  await expect(page.locator("#review-file-beta_txt .review-rows")).toBeVisible();

  const before = await railWidth(page);
  const resizer = page.getByRole("separator", { name: "resize file list" });
  const handle = (await resizer.boundingBox())!;
  await page.mouse.move(handle.x + handle.width / 2, handle.y + 200);
  await page.mouse.down();
  await page.mouse.move(handle.x + handle.width / 2 + 140, handle.y + 200, { steps: 8 });
  await page.mouse.up();
  const widened = await railWidth(page);
  expect(widened).toBeGreaterThan(before + 100);

  await page.reload();
  await expect(page.locator(".review-rail-file").first()).toBeVisible();
  expect(Math.abs((await railWidth(page)) - widened)).toBeLessThanOrEqual(2);
});

test("the file list says whose turn each file's threads are on and jumps to them", async ({
  page,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const reviewId = openReview(session!.id, seedWorktree());
  isolatedDaemon.seedReviewThreads(reviewId, [
    // Two answers waiting on the reader, one comment still with the agent.
    { path: "alpha.txt", line: 9, state: "answered", messages: [
      { author: "user", body: "why this order?" },
      { author: "session", body: "kept it for the caller." },
    ] },
    { path: "alpha.txt", line: 61, state: "sent", messages: [
      { author: "user", body: "can this take the worktree?" },
    ] },
    { path: "alpha.txt", line: 101, state: "answered", messages: [
      { author: "user", body: "spelling" },
      { author: "session", body: "fixed the wording." },
    ] },
    // Written and never sent, so the move left on it is the reader's own.
    { path: "beta.txt", line: 13, state: "draft", messages: [
      { author: "user", body: "half a thought" },
    ] },
    // Closed, with nothing in it the reader has not read.
    { path: "gamma.txt", line: 5, state: "resolved", messages: [
      { author: "user", body: "never mind" },
    ] },
  ]);

  await openReviewPage(page, reviewId);

  // The head counts the same states the circles do, named by whose turn
  // it is rather than by what a thread is called.
  const head = page.locator(".review-head-top");
  await expect(head.locator(".badge.st-needs-input")).toHaveText("2 your turn");
  await expect(head.locator(".badge.st-working")).toHaveText("1 with agent");
  await expect(head.locator(".badge.st-starting")).toHaveText("1 draft");
  await expect(head.locator(".badge.st-idle")).toHaveText("1 resolved");

  // Alpha holds two the agent answered and one it has not, so its open
  // threads split rather than sitting under one number.
  const alpha = row(page, "alpha.txt");
  await expect(alpha.locator(".review-rail-count.is-yours")).toHaveText("2");
  await expect(alpha.locator(".review-rail-count.is-theirs")).toHaveText("1");
  await expect(alpha.locator(".review-rail-count.is-resolved")).toHaveCount(0);

  // The agent has never been handed the draft, so nothing is pending on
  // it: the circle says the reader owes the move, which is to send it.
  const beta = row(page, "beta.txt");
  await expect(beta.locator(".review-rail-count.is-yours")).toHaveText("1");
  await expect(beta.locator(".review-rail-count.is-theirs")).toHaveCount(0);

  // Closed threads get their own circle rather than disappearing.
  const gamma = row(page, "gamma.txt");
  await expect(gamma.locator(".review-rail-count.is-resolved")).toHaveText("1");
  await expect(gamma.locator(".review-rail-count.is-yours")).toHaveCount(0);
  await expect(gamma.locator(".review-rail-count.is-theirs")).toHaveCount(0);

  // A file nobody has commented on carries no circle at all.
  await expect(row(page, "delta.txt").locator(".review-rail-count")).toHaveCount(0);

  // Every circle in the rail reconciles against the head, so a reader
  // reading both is never told two different things: the amber circles
  // carry the drafts along with the answers waiting on the reader.
  const rail = page.locator(".review-rail");
  expect(await total(rail.locator(".review-rail-count.is-yours"))).toBe(2 + 1);
  expect(await total(rail.locator(".review-rail-count.is-theirs"))).toBe(1);
  expect(await total(rail.locator(".review-rail-count.is-resolved"))).toBe(1);

  const first = page.locator(".review-thread", { hasText: "kept it for the caller." });
  const middle = page.locator(".review-thread", { hasText: "can this take the worktree?" });
  const last = page.locator(".review-thread", { hasText: "fixed the wording." });
  await expect(last).not.toBeInViewport();

  // Each circle walks its own state's threads in line order, one per
  // click, and wraps within that state rather than crossing into the
  // other one.
  const waitingOnYou = alpha.getByRole("button", { name: /your turn — go to 2 threads in alpha\.txt/ });
  await waitingOnYou.click();
  await expect(first).toBeInViewport();
  await waitingOnYou.click();
  await expect(last).toBeInViewport();
  await expect(middle).not.toBeInViewport();
  await waitingOnYou.click();
  await expect(first).toBeInViewport();

  // The thread still with the agent is reached by its own circle.
  const waitingOnAgent = alpha.getByRole("button", { name: /with agent — go to 1 thread in alpha\.txt/ });
  await waitingOnAgent.click();
  await expect(middle).toBeInViewport();

  // The resolved circle walks its own group, and clicking it does not
  // resume where the open circle left off.
  const closed = page.locator(".review-thread", { hasText: "never mind" });
  await gamma.getByRole("button", { name: /resolved — go to 1 thread in gamma\.txt/ }).click();
  await expect(closed).toBeInViewport();

  // n still walks every open thread whoever it waits on, so splitting the
  // circles has not split the review-wide walk. It crosses files, and it
  // reaches the draft because a draft is open until somebody closes it.
  const draft = page.locator(".review-thread", { hasText: "half a thought" });
  await page.keyboard.press("n");
  await expect(first).toBeInViewport();
  await page.keyboard.press("n");
  await expect(middle).toBeInViewport();
  await page.keyboard.press("n");
  await expect(last).toBeInViewport();
  await page.keyboard.press("n");
  await expect(draft).toBeInViewport();

  // Past the last open thread it wraps, and Shift+N wraps the other way.
  await page.keyboard.press("n");
  await expect(first).toBeInViewport();
  await page.keyboard.press("Shift+N");
  await expect(draft).toBeInViewport();
});

test("the file list folds to a rail and comes back at the width it had", async ({
  page,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const reviewId = openReview(session!.id, seedWorktree());

  await openReviewPage(page, reviewId);

  // Widen it first, so restoring a default would be visible as a failure.
  const resizer = page.getByRole("separator", { name: "resize file list" });
  const handle = (await resizer.boundingBox())!;
  await page.mouse.move(handle.x + handle.width / 2, handle.y + 200);
  await page.mouse.down();
  await page.mouse.move(handle.x + handle.width / 2 + 120, handle.y + 200, { steps: 8 });
  await page.mouse.up();
  const chosen = await railWidth(page);
  expect(chosen).toBeGreaterThan(300);

  const fold = page.getByRole("button", { name: "hide the file list" });
  await fold.click();

  // A rail, not a zero-width column: the control that brings the list
  // back has to stay reachable.
  const folded = await railWidth(page);
  expect(folded).toBeGreaterThan(0);
  expect(folded).toBeLessThan(60);
  await expect(page.locator(".review-rail-file")).toHaveCount(0);
  // Nothing to drag while it is folded.
  await expect(resizer).toHaveCount(0);
  // The diff takes the space the list gave up.
  expect(await page.locator(".review-diff").evaluate((el) => el.clientWidth)).toBeGreaterThan(0);

  // Folded is a preference, so it outlives the page.
  await page.reload();
  const unfold = page.getByRole("button", { name: "show the file list" });
  await expect(unfold).toBeVisible();
  await expect(page.locator(".review-rail-file")).toHaveCount(0);
  expect(await railWidth(page)).toBeLessThan(60);

  // Coming back returns the width the reader dragged to, not a default.
  await unfold.click();
  await expect(page.locator(".review-rail-file").first()).toBeVisible();
  expect(Math.abs((await railWidth(page)) - chosen)).toBeLessThanOrEqual(2);
  await expect(page.getByRole("separator", { name: "resize file list" })).toBeVisible();

  // And expanded outlives the page too.
  await page.reload();
  await expect(page.locator(".review-rail-file").first()).toBeVisible();
  expect(Math.abs((await railWidth(page)) - chosen)).toBeLessThanOrEqual(2);
});

// The file list and the session list share one drag, so a change to it
// has to leave the session list resizing and persisting as it did.
test("the session list keeps its own resize and stored width", async ({ page }) => {
  await logIn(page);
  const sidebar = page.locator(".sidebar-shell");
  const before = await sidebar.evaluate((el) => el.getBoundingClientRect().width);
  const resizer = page.getByRole("separator", { name: "resize sidebar" });
  const handle = (await resizer.boundingBox())!;
  await page.mouse.move(handle.x + handle.width / 2, handle.y + 200);
  await page.mouse.down();
  await page.mouse.move(handle.x + handle.width / 2 + 90, handle.y + 200, { steps: 8 });
  await page.mouse.up();
  const widened = await sidebar.evaluate((el) => el.getBoundingClientRect().width);
  expect(widened).toBeGreaterThan(before + 50);

  await page.reload();
  await expect(page.locator(".sb-session").first()).toBeVisible();
  const restored = await sidebar.evaluate((el) => el.getBoundingClientRect().width);
  expect(Math.abs(restored - widened)).toBeLessThanOrEqual(2);
});

test("marking a file viewed lands on the next unviewed file, not past it", async ({
  page,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const reviewId = openReview(session!.id, seedWorktree());

  await openReviewPage(page, reviewId);
  // The review lists files in its own order, which is not the order they
  // were written here, so the walk follows the rail rather than guessing.
  const order = await page.locator(".review-rail-name").allInnerTexts();
  expect(order).toHaveLength(FILES.length);
  const sectionId = (file: string) => `review-file-${file.replace(/[^a-zA-Z0-9_-]/g, "_")}`;
  const viewedBox = (file: string) =>
    page.locator(`#${sectionId(file)}`).getByRole("checkbox", { name: "Viewed" });

  // Each file cleared puts the next one still needing attention at the
  // top, rather than scrolling past it by the height the collapse just
  // took out of the document.
  for (let i = 0; i < order.length - 1; i += 1) {
    await viewedBox(order[i]).click();
    await expect(page.locator(`#${sectionId(order[i])} .review-rows`)).toHaveCount(0);
    await expect.poll(() => headerOffset(page, order[i + 1])).toBeLessThanOrEqual(2);
    expect(await headerOffset(page, order[i + 1])).toBeGreaterThanOrEqual(-2);
  }

  // Reopening a file leaves the reader on that file. Only marking one
  // viewed moves them on, so the file after it must not come to the top.
  await viewedBox(order[0]).click();
  await expect(page.locator(`#${sectionId(order[0])} .review-rows`)).toBeVisible();
  expect(await headerOffset(page, order[0])).toBeLessThanOrEqual(2);
  expect(await headerOffset(page, order[1])).toBeGreaterThan(100);
});

test("clearing the last file holds the reader there rather than throwing them back up", async ({
  page,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const reviewId = openReview(session!.id, seedWorktree());

  await openReviewPage(page, reviewId);
  const order = await page.locator(".review-rail-name").allInnerTexts();
  const last = order[order.length - 1];
  const sectionId = (file: string) => `review-file-${file.replace(/[^a-zA-Z0-9_-]/g, "_")}`;
  const lastBox = page.locator(`#${sectionId(last)}`).getByRole("checkbox", { name: "Viewed" });

  // Every file above is still unviewed, so the old wrap would have sent
  // the reader back to the top of the review.
  await page.locator(".review-rail-file", { hasText: last }).click();
  await expect.poll(() => headerOffset(page, last)).toBeLessThanOrEqual(2);
  await lastBox.click();
  await expect(page.locator(`#${sectionId(last)} .review-rows`)).toHaveCount(0);
  // Collapsing the last file takes its body out of the document, so its
  // header can no longer reach the top — there is nothing left below it
  // to scroll. It stays on screen at the end of the review, and the
  // files above stay above.
  await expect.poll(() => atDocumentEnd(page)).toBe(true);
  const rest = await headerOffset(page, last);
  expect(rest).toBeGreaterThanOrEqual(0);
  expect(rest).toBeLessThan(await viewportHeight(page));
  expect(await headerOffset(page, order[0])).toBeLessThan(-1000);

});

// Reading a review writes the reader's own state constantly: a file
// marked viewed, a scroll position, a draft. Each of those replaced the
// page's review detail, and the diff was fetched whenever that object
// changed, so every one of them refetched, reparsed and re-highlighted
// every file in the review. The diff only depends on the view selector,
// the context width and the revision being read, so nothing the reader
// does to their own state should fetch it again.
test("the reader's own state does not refetch the diff", async ({ page, isolatedDaemon }) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const reviewId = openReview(session!.id, seedWorktree());

  const diffRequests: string[] = [];
  page.on("request", (request) => {
    if (/\/api\/reviews\/\d+\/diff\?/.test(request.url())) diffRequests.push(request.url());
  });

  await openReviewPage(page, reviewId);
  const alphaRows = page.locator("#review-file-alpha_txt .review-rows");
  await expect(alphaRows).toBeVisible();
  const onLoad = diffRequests.length;
  expect(onLoad).toBeGreaterThan(0);

  await page.locator("#review-file-alpha_txt").getByRole("checkbox", { name: "Viewed" }).click();
  await expect(alphaRows).toHaveCount(0);
  expect(diffRequests).toHaveLength(onLoad);

  await page.locator("#review-file-alpha_txt").getByRole("checkbox", { name: "Viewed" }).click();
  await expect(alphaRows).toBeVisible();
  expect(diffRequests).toHaveLength(onLoad);

  // Changing how much context the daemon renders is a different diff, so
  // that one does fetch.
  await page.getByRole("combobox", { name: "context" }).selectOption("3");
  await expect.poll(() => diffRequests.length).toBe(onLoad + 1);
});
