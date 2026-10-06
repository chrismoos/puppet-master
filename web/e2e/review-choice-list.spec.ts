import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "./fixtures";
import { accessToken, logIn } from "./support";

// A plan carrying both kinds of task list: one an agent marked as a
// question, one a real todo list nobody should be asked to answer.
const PLAN = [
  "# Auth",
  "",
  "How should sessions work?",
  "",
  "<!-- pm-choice id=auth-approach select=one -->",
  "- [ ] JWT with refresh rotation",
  "  Stateless, but revocation needs a denylist.",
  "- [ ] Server-side sessions",
  "  Trivial revocation, needs sticky routing.",
  "- [ ] Other",
  "",
  "## Still to do",
  "",
  "- [ ] write the migration",
  "- [x] read the spec",
  "",
].join("\n");

function pm(args: string[]): string {
  return execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
}

function seedWorktree(): string {
  const dir = mkdtempSync(join(tmpdir(), "pm-choice-"));
  const git = (...args: string[]) =>
    execFileSync("git", args, {
      cwd: dir,
      encoding: "utf8",
      env: { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" },
    });
  git("init", "-q", "-b", "main");
  git("config", "user.email", "e2e@example.invalid");
  git("config", "user.name", "browser e2e");
  writeFileSync(join(dir, "plan.md"), "# Auth\n");
  git("add", "-A");
  git("commit", "-q", "-m", "base");
  writeFileSync(join(dir, "plan.md"), PLAN);
  return dir;
}

function openReview(sessionId: number, worktree: string): number {
  const out = pm(["review", "open", String(sessionId), "--worktree", worktree, "--label", "plan"]);
  const id = out.match(/review (\d+) open/)?.[1];
  if (!id) throw new Error(`could not read a review id from: ${out}`);
  return Number(id);
}

interface ApiAnswer {
  choice_id: string;
  select: string;
  option_ids: string[];
  option_labels: string[];
  other_text: string;
  notes: string;
}

/** Every answer the review holds, read back the way the agent gets it:
 *  as fields on a message, not as a sentence to parse. */
async function storedAnswers(page: Page, reviewId: number): Promise<ApiAnswer[]> {
  const bearer = await accessToken(page);
  return page.evaluate(async ([id, token]: [number, string]) => {
    const res = await fetch(`/api/reviews/${id}`, {
      cache: "no-store",
      headers: { Authorization: `Bearer ${token}` },
    });
    const detail = await res.json();
    return detail.threads.flatMap((t: { messages: { choice: ApiAnswer | null }[] }) =>
      t.messages.map((m) => m.choice).filter(Boolean),
    );
  }, [reviewId, bearer] as [number, string]);
}

async function openPlan(page: Page, reviewId: number): Promise<void> {
  await logIn(page);
  await page.goto(`${process.env.PM_E2E_BASE_URL}/#/review/${reviewId}`);
  await expect(page.locator(".review-choice")).toBeVisible();
}

test("a marked option list is answerable, an unmarked task list is not", async ({
  page,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const reviewId = openReview(session!.id, seedWorktree());

  await openPlan(page, reviewId);

  const card = page.locator(".review-choice");
  await expect(card.locator(".review-choice-id")).toHaveText("auth-approach");
  await expect(card.locator(".review-choice-mode")).toHaveText("choose one");

  // Every option, with the indented lines under it as that option's own
  // detail rather than as loose paragraphs after the list.
  const options = card.locator(".review-choice-options li");
  await expect(options).toHaveCount(3);
  await expect(options.nth(0).locator(".review-choice-label")).toHaveText(
    "JWT with refresh rotation",
  );
  await expect(options.nth(0).locator(".review-choice-detail")).toHaveText(
    "Stateless, but revocation needs a denylist.",
  );
  // select=one, so radios.
  await expect(options.nth(0).getByRole("radio")).toBeVisible();

  // The one thing that cannot be designed away: the plan's real todo
  // list stays a todo list, with no control anywhere near it.
  const todo = page.locator(".review-preview .markdown li", { hasText: "write the migration" });
  await expect(todo).toHaveText("[ ] write the migration");
  await expect(todo.locator("input")).toHaveCount(0);
  await expect(page.locator(".review-choice")).toHaveCount(1);
});

test("a selection batches with the other comments, and can be sent on its own", async ({
  page,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const reviewId = openReview(session!.id, seedWorktree());

  await openPlan(page, reviewId);
  const card = page.locator(".review-choice");
  const chosen = card.locator(".review-choice-options li", { hasText: "Server-side sessions" });

  // Nothing is waiting to be sent before the reader answers.
  await expect(page.getByRole("button", { name: /Send \d+ draft/ })).toHaveCount(0);
  await expect(card.locator(".review-choice-send")).toHaveCount(0);

  await chosen.getByRole("radio").check();

  // The answer becomes a draft, so it travels with the reader's other
  // comments when they send the pass.
  await expect(page.getByRole("button", { name: "Send 1 draft" })).toBeVisible();
  // And the chosen option stays marked, so the plan records its own
  // decision rather than resetting to an unanswered question.
  await expect(chosen).toHaveClass(/is-picked/);
  await expect(chosen.getByRole("radio")).toBeChecked();

  await card.getByRole("textbox", { name: "notes on auth-approach" }).fill("sticky routing is fine");
  await expect
    .poll(() => storedAnswers(page, reviewId).then((a) => a[0]?.notes))
    .toBe("sticky routing is fine");

  // One decision can go on its own without sending the rest of the pass.
  await card.getByRole("button", { name: "send this answer now" }).click();
  await expect(page.getByRole("button", { name: /Send \d+ draft/ })).toHaveCount(0);
  await expect(card.locator(".review-choice-state")).toHaveText("sent");

  // What the agent receives is the decision as fields, not prose.
  const answers = await storedAnswers(page, reviewId);
  expect(answers).toHaveLength(1);
  expect(answers[0]).toMatchObject({
    choice_id: "auth-approach",
    select: "one",
    option_ids: ["server-side-sessions"],
    option_labels: ["Server-side sessions"],
    notes: "sticky routing is fine",
  });

  // The answer survives the page, and stays a record rather than a
  // control once the agent has it.
  await page.reload();
  const reloaded = page.locator(".review-choice");
  await expect(reloaded.locator(".review-choice-state")).toHaveText("sent");
  await expect(
    reloaded.locator(".review-choice-options li", { hasText: "Server-side sessions" }),
  ).toHaveClass(/is-picked/);
  await expect(reloaded.getByRole("radio").first()).toBeDisabled();
});

test("choosing Other asks what instead, and refuses to send until it is told", async ({
  page,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const reviewId = openReview(session!.id, seedWorktree());

  await openPlan(page, reviewId);
  const card = page.locator(".review-choice");
  await card
    .locator(".review-choice-options li", { hasText: "Other" })
    .getByRole("radio")
    .check();

  const other = card.getByRole("textbox", { name: "the other option" });
  await expect(other).toBeVisible();
  // "Other" with nothing written in it tells the agent nothing, so
  // there is nothing to send yet.
  await expect(card.locator(".review-choice-send")).toHaveCount(0);

  await other.fill("mTLS between services");
  await expect(card.locator(".review-choice-send")).toBeVisible();
  await card.getByRole("button", { name: "send this answer now" }).click();

  await expect.poll(() => storedAnswers(page, reviewId).then((a) => a.length)).toBe(1);
  const answers = await storedAnswers(page, reviewId);
  expect(answers[0]).toMatchObject({
    option_ids: ["other"],
    other_text: "mTLS between services",
  });
});
