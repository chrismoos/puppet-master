import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

// Reading down a long review scrolled the diff past the end of the file
// list, so the file on screen was no longer in the rail at all.

const FILES = Array.from({ length: 30 }, (_, i) => `file-${String(i).padStart(2, "0")}.txt`);
const LINES = 60;

function pm(args: string[]): string {
  return execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
}

function seedWorktree(): string {
  const dir = mkdtempSync(join(tmpdir(), "pm-rail-"));
  const git = (...args: string[]) =>
    execFileSync("git", args, {
      cwd: dir,
      encoding: "utf8",
      env: { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" },
    });
  const body = (name: string, edited: boolean) =>
    Array.from({ length: LINES }, (_, i) =>
      edited && i % 3 === 0 ? `${name} line ${i + 1} edited` : `${name} line ${i + 1}`,
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
  const out = pm(["review", "open", String(sessionId), "--worktree", worktree, "--label", "rail"]);
  const id = out.match(/review (\d+) open/)?.[1];
  if (!id) throw new Error(`could not read a review id from: ${out}`);
  return Number(id);
}

/** Whether a rail row sits inside the rail's own scroll box. */
async function rowVisible(page: Page, file: string): Promise<boolean> {
  return page.evaluate((path) => {
    const rail = document.querySelector<HTMLElement>(".review-rail");
    const row = rail?.querySelector<HTMLElement>(`[data-rail-file="${CSS.escape(path)}"]`);
    if (!rail || !row) return false;
    const top = row.offsetTop;
    const bottom = top + row.offsetHeight;
    return top >= rail.scrollTop - 1 && bottom <= rail.scrollTop + rail.clientHeight + 1;
  }, file);
}

async function readingFile(page: Page): Promise<string> {
  return page.evaluate(() => {
    const diff = document.querySelector<HTMLElement>(".review-diff");
    if (!diff) return "";
    const top = diff.getBoundingClientRect().top;
    let current = "";
    for (const section of document.querySelectorAll<HTMLElement>(".review-file")) {
      if (section.getBoundingClientRect().top - top <= 1) current = section.dataset.file ?? current;
    }
    return current;
  });
}

test("the file list follows the file being read", async ({ page, isolatedDaemon }) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const reviewId = openReview(session!.id, seedWorktree());

  await logIn(page);
  await page.setViewportSize({ width: 1280, height: 620 });
  await page.goto(`${process.env.PM_E2E_BASE_URL}/#/review/${reviewId}`);
  await expect(page.locator(".review-rail-file").first()).toBeVisible();

  // The rail cannot hold thirty files at this height, which is the whole
  // point: reading past its end used to leave the current file off it.
  const rows = page.locator(".review-rail-file");
  await expect(rows).toHaveCount(FILES.length);
  expect(await rowVisible(page, FILES[FILES.length - 1])).toBe(false);

  await page.locator(".review-diff").evaluate((el) => {
    el.scrollTop = el.scrollHeight;
  });

  await expect.poll(async () => readingFile(page), { timeout: 15_000 }).not.toBe("");
  const reading = await readingFile(page);
  expect(FILES).toContain(reading);
  await expect
    .poll(async () => rowVisible(page, reading), { timeout: 15_000 })
    .toBe(true);
});
