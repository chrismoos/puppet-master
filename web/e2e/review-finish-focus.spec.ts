import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test } from "./fixtures";
import { logIn } from "./support";

// Focus mode strips the shell down to read something. Finishing the review
// takes away the thing being read, so staying stripped down leaves the
// reader somewhere they never chose.

function pm(args: string[]): string {
  return execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
}

function seedWorktree(): string {
  const dir = mkdtempSync(join(tmpdir(), "pm-finish-"));
  const git = (...args: string[]) =>
    execFileSync("git", args, {
      cwd: dir,
      encoding: "utf8",
      env: { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" },
    });
  git("init", "-q", "-b", "main");
  git("config", "user.email", "e2e@example.invalid");
  git("config", "user.name", "browser e2e");
  writeFileSync(join(dir, "a.txt"), "one\ntwo\nthree\n");
  git("add", "-A");
  git("commit", "-q", "-m", "base");
  writeFileSync(join(dir, "a.txt"), "one\nTWO\nthree\n");
  return dir;
}

test("finishing a review leaves focus mode", async ({ page, isolatedDaemon }) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const out = pm(["review", "open", String(session!.id), "--worktree", seedWorktree(), "--label", "finish"]);
  const reviewId = Number(out.match(/review (\d+) open/)![1]);

  await logIn(page);
  await page.locator(".sb-session").first().click();
  await expect(page.locator(".xterm")).toBeVisible();

  await page.getByRole("button", { name: "enter focus mode" }).click();
  await expect(page.locator(".shell")).toHaveClass(/is-focus-mode/);

  await page.goto(`${process.env.PM_E2E_BASE_URL}/#/review/${reviewId}`);
  await expect(page.locator(".review-rail-file").first()).toBeVisible();
  await expect(page.locator(".shell")).toHaveClass(/is-focus-mode/);

  await page.getByRole("button", { name: "Finish review" }).click();

  await expect(page.locator(".shell")).not.toHaveClass(/is-focus-mode/);
});

test("a review tab opens the review in its own window and leaves the pane alone", async ({ page, context, isolatedDaemon }) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  pm(["review", "open", String(session!.id), "--worktree", seedWorktree(), "--label", "own-window"]);

  await logIn(page);
  await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title", { hasText: /^browser-e2e$/ }) }).click();
  await expect(page.locator(".xterm")).toBeVisible();
  await expect(page.locator(".shell")).not.toHaveClass(/is-focus-mode/);

  // A real link, which is what makes middle-click, a modifier click and
  // the context menu's own open-in-new-tab work.
  const tab = page.locator(".review-tab").first();
  await expect(tab).toHaveAttribute("target", "_blank");
  await expect(tab).toHaveAttribute("rel", "noreferrer");
  await expect(tab).toHaveAttribute("href", /#\/session\/\d+\?tab=review%3A\d+&focus=1$/);

  const [opened] = await Promise.all([context.waitForEvent("page"), tab.click()]);
  await expect(opened.locator(".review-rail-file").first()).toBeVisible();
  await expect(opened.locator(".shell")).toHaveClass(/is-focus-mode/);

  // The window that held the link never moved, which is the point of
  // opening the review somewhere else.
  await expect(page.locator(".shell")).not.toHaveClass(/is-focus-mode/);
  await expect(page.locator(".xterm")).toBeVisible();
  await expect(page).toHaveURL(/#\/session\/\d+$/);

  // Leaving focus mode in the review's own window puts the reader on the
  // session's agent, not on a review tab with nothing behind it.
  await expect(opened.getByRole("button", { name: "exit focus mode" })).toBeVisible();
  await opened.getByRole("button", { name: "exit focus mode" }).click();
  await expect(opened.locator(".shell")).not.toHaveClass(/is-focus-mode/);
  await expect(opened.locator(".xterm")).toBeVisible();
  await expect(opened).toHaveURL(/#\/session\/\d+$/);
  await opened.close();
});

test("finishing a review in its own window closes it", async ({ page, context, isolatedDaemon }) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  pm(["review", "open", String(session!.id), "--worktree", seedWorktree(), "--label", "finish-window"]);

  let terminalConnections = 0;
  let flashedTerminal = false;
  context.on("page", (child) => {
    child.on("websocket", (socket) => {
      if (socket.url().includes("/ws/terminal/")) terminalConnections += 1;
    });
    child.on("console", (message) => {
      if (message.text() === "review-terminal-mounted") flashedTerminal = true;
    });
  });
  await context.addInitScript(() => {
    new MutationObserver(() => {
      if (document.querySelector(".xterm")) console.log("review-terminal-mounted");
    }).observe(document, { childList: true, subtree: true });
  });
  await logIn(page);
  await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title", { hasText: /^browser-e2e$/ }) }).click();
  await expect(page.locator(".xterm")).toBeVisible();

  const [opened] = await Promise.all([
    context.waitForEvent("page"),
    page.locator(".review-tab").first().click(),
  ]);
  await opened.setViewportSize({ width: 900, height: 600 });
  await expect(opened.locator(".review-rail-file").first()).toBeVisible();
  await expect(opened.locator(".xterm, .sidebar, .topbar, .terminal-tabs")).toHaveCount(0);
  await expect(page.locator(".terminal-size-prompt")).toBeHidden();
  expect(terminalConnections).toBe(0);
  const bounds = await opened.locator(".review").boundingBox();
  expect(bounds).toEqual({ x: 0, y: 0, width: 900, height: 600 });

  // A deliberate move inside the review, which is what a reader does before
  // finishing and what makes a window the browser opened refuse to close.
  await opened.locator(".review-rail-file").first().click();

  await Promise.all([
    opened.waitForEvent("close"),
    opened.getByRole("button", { name: "Finish review" }).click(),
  ]);
  expect(opened.isClosed()).toBe(true);
  expect(terminalConnections).toBe(0);
  expect(flashedTerminal).toBe(false);
  await expect(page.locator(".terminal-size-prompt")).toBeHidden();

  // The window that opened the review is left where it was.
  await expect(page.locator(".xterm")).toBeVisible();
  await expect(page).toHaveURL(/#\/session\/\d+$/);
});

test("a review in its own window takes comments", async ({ page, context, isolatedDaemon }) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  pm(["review", "open", String(session!.id), "--worktree", seedWorktree(), "--label", "comment-window"]);

  await logIn(page);
  await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title", { hasText: /^browser-e2e$/ }) }).click();
  await expect(page.locator(".xterm")).toBeVisible();

  const [opened] = await Promise.all([
    context.waitForEvent("page"),
    page.locator(".review-tab").first().click(),
  ]);
  await expect(opened.locator(".review-rail-file").first()).toBeVisible();

  const file = opened.locator("#review-file-a_txt");
  await file.locator(".review-row", { hasText: /TWO/ }).first().click();
  const composer = file.locator(".review-compose");
  await expect(composer).toBeVisible();
  await composer.locator("textarea").fill("this reads better shouted");
  await composer.getByRole("button", { name: "Save as draft" }).click();

  await expect(file.locator(".review-thread", { hasText: "this reads better shouted" }))
    .toBeVisible();
  await opened.close();
});

test("pressing Escape in a review window exits focus mode and returns to the agent", async ({ page, context, isolatedDaemon }) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  pm(["review", "open", String(session!.id), "--worktree", seedWorktree(), "--label", "escape-window"]);

  await logIn(page);
  await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title", { hasText: /^browser-e2e$/ }) }).click();
  await expect(page.locator(".xterm")).toBeVisible();

  const [opened] = await Promise.all([
    context.waitForEvent("page"),
    page.locator(".review-tab").first().click(),
  ]);
  await expect(opened.locator(".shell")).toHaveClass(/is-focus-mode/);

  await opened.keyboard.press("Escape");
  await expect(opened.locator(".shell")).not.toHaveClass(/is-focus-mode/);
  await expect(opened.locator(".xterm")).toBeVisible();
  await opened.close();
});

