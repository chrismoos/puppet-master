import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { findBuiltinTerminalTheme } from "@puppet-master/client-core/theme/builtinTerminalThemes";
import { expect, test, type Page } from "./fixtures";
import { apiHeaders, logIn } from "./support";

const SOURCE = "example.ts";

function pm(args: string[]): string {
  return execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
}

/// Source with a comment, a string and a keyword on separate lines, so
/// each token role the palette resolves is actually painted.
function body(edited: boolean): string {
  return [
    "// counts the retries a session has left",
    "export function retriesLeft(limit: number): number {",
    `  const label = "retries";`,
    `  return ${edited ? "limit - 1" : "limit"};`,
    "}",
    "",
  ].join("\n");
}

function seedWorktree(): string {
  const dir = mkdtempSync(join(tmpdir(), "pm-code-surface-"));
  const git = (...args: string[]) =>
    execFileSync("git", args, {
      cwd: dir,
      encoding: "utf8",
      env: { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" },
    });
  git("init", "-q", "-b", "main");
  git("config", "user.email", "e2e@example.invalid");
  git("config", "user.name", "browser e2e");
  writeFileSync(join(dir, SOURCE), body(false));
  git("add", "-A");
  git("commit", "-q", "-m", "base");
  writeFileSync(join(dir, SOURCE), body(true));
  return dir;
}

function openReview(sessionId: number, worktree: string): number {
  const out = pm(["review", "open", String(sessionId), "--worktree", worktree, "--label", "code surface"]);
  const id = out.match(/review (\d+) open/)?.[1];
  if (!id) throw new Error(`could not read a review id from: ${out}`);
  return Number(id);
}

/// The colour a reader sees a token against, and the ratio between them.
async function tokenContrast(page: Page, selector: string) {
  return page.evaluate((token) => {
    const parse = (value: string) => {
      const parts = value.match(/[\d.]+/g);
      if (!parts) return null;
      const alpha = parts.length > 3 ? Number(parts[3]) : 1;
      return alpha === 0 ? null : [Number(parts[0]), Number(parts[1]), Number(parts[2])];
    };
    const luminance = ([r, g, b]: number[]) => {
      const linear = [r, g, b].map((channel) => {
        const c = channel / 255;
        return c <= 0.04045 ? c / 12.92 : ((c + 0.055) / 1.055) ** 2.4;
      });
      return 0.2126 * linear[0] + 0.7152 * linear[1] + 0.0722 * linear[2];
    };
    const element = document.querySelector(token);
    if (!element) return null;
    let behind: number[] | null = null;
    for (let node: Element | null = element; node; node = node.parentElement) {
      behind = parse(getComputedStyle(node).backgroundColor);
      if (behind) break;
    }
    const color = parse(getComputedStyle(element).color)!;
    const a = luminance(color);
    const b = luminance(behind ?? [255, 255, 255]);
    return {
      color: getComputedStyle(element).color,
      behind: `rgb(${(behind ?? [255, 255, 255]).join(", ")})`,
      ratio: Number((((Math.max(a, b) + 0.05) / (Math.min(a, b) + 0.05))).toFixed(2)),
    };
  }, selector);
}

function rgb(hex: string): string {
  const [r, g, b] = [1, 3, 5].map((start) => Number.parseInt(hex.slice(start, start + 2), 16));
  return `rgb(${r}, ${g}, ${b})`;
}

async function applyTerminalTheme(page: Page, name: string): Promise<void> {
  const theme = findBuiltinTerminalTheme(name)!;
  const response = await page.request.put(
    `${process.env.PM_E2E_BASE_URL}/api/user/settings/terminal-theme`,
    { data: theme, headers: await apiHeaders(page) },
  );
  expect(response.ok()).toBe(true);
}

/// The pairing the two independent settings make possible: whichever way
/// round they are set, the diff is painted from the terminal palette and
/// every token still reads on it.
async function expectCodeSurfaceFollowsTerminal(page: Page, themeName: string, reviewId: number) {
  const theme = findBuiltinTerminalTheme(themeName)!;
  await applyTerminalTheme(page, themeName);
  await page.goto(`${process.env.PM_E2E_BASE_URL}/#/review/${reviewId}`);
  await expect(page.locator(".review-rows").first()).toBeVisible();
  await expect(page.locator(".review-code .hljs-comment").first()).toBeVisible();

  await expect(page.locator(".review-rows").first())
    .toHaveCSS("background-color", rgb(theme.colors.background));

  for (const token of ["hljs-comment", "hljs-keyword", "hljs-string"]) {
    const measured = await tokenContrast(page, `.review-code .${token}`);
    expect(measured, `${themeName} ${token}`).not.toBeNull();
    expect(measured!.ratio, `${themeName} ${token} on ${measured!.behind}`).toBeGreaterThanOrEqual(3);
  }

  // The added row is tinted, and its tint is the palette's, not the page's.
  const added = await page.locator(".review-row.is-add").first().evaluate(
    (element) => getComputedStyle(element).backgroundColor,
  );
  expect(added).not.toBe(rgb(theme.colors.background));
}

for (const appearance of ["light", "dark"] as const) {
  test.describe(`with the page in ${appearance}`, () => {
    test.use({ colorScheme: appearance });

    test("the review diff is painted by the terminal palette, both ways round", async ({
      page,
      isolatedDaemon,
    }) => {
      const session = isolatedDaemon.session("browser-e2e");
      expect(session).not.toBeNull();
      const reviewId = openReview(session!.id, seedWorktree());
      await logIn(page);

      await expectCodeSurfaceFollowsTerminal(page, "Solarized Light", reviewId);
      await expectCodeSurfaceFollowsTerminal(page, "Dracula", reviewId);
    });
  });
}
