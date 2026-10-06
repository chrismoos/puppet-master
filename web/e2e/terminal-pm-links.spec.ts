import { execFileSync } from "node:child_process";
import { mkdtempSync, writeFileSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

interface LinkReturnStage {
  debugSnapshot(): Array<{
    key: string;
    visible: boolean;
    baseY: number;
    viewportY: number;
  }>;
  layers: Map<string, {
    el: HTMLElement;
    term: {
      cols: number;
      rows: number;
      buffer: { active: {
        baseY: number;
        viewportY: number;
        getLine(row: number): { translateToString(trimRight?: boolean): string } | undefined;
      } };
      clearSelection(): void;
      getSelection(): string;
      scrollToLine(row: number): void;
    };
  }>;
}

type LinkReturnWindow = Window & { __pmStage?: LinkReturnStage };

function cli(args: string[]): string {
  return execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
}

/** A route change moves the stage's visible layer on a later render, so a
 * visible-filtered locator can resolve to the outgoing terminal and split one
 * typed command across two of them. */
async function expectRoutedTerminalVisible(page: Page): Promise<void> {
  const sessionId = page.url().match(/#\/session\/(\d+)$/)?.[1];
  if (!sessionId) return;
  await expect.poll(() => page.evaluate(() => (window as LinkReturnWindow).__pmStage
    ?.debugSnapshot().filter((candidate) => candidate.visible).map((candidate) => candidate.key) ?? []), {
    message: `waiting for session ${sessionId} to own the visible terminal layer`,
  }).toEqual([`s:${sessionId}`]);
}

async function writeReference(
  page: Page,
  host: string,
  reference: string,
  expectedHref = reference,
): Promise<{ x: number; y: number; cellWidth: number }> {
  await expectRoutedTerminalVisible(page);
  const terminal = page.locator(`${host} .xterm`).filter({ visible: true });
  const textarea = terminal.locator(".xterm-helper-textarea");
  await textarea.focus();
  await textarea.pressSequentially(`echo ${reference}`);
  await textarea.press("Enter");
  let found: { x: number; y: number; cellWidth: number } | undefined;
  await expect.poll(async () => {
    const geometry = await terminal.evaluate((element) => {
      const screen = element.querySelector<HTMLElement>(".xterm-screen")!;
      const input = element.querySelector<HTMLTextAreaElement>(".xterm-helper-textarea")!;
      const screenRect = screen.getBoundingClientRect();
      const cellWidth = input.getBoundingClientRect().width;
      const cellHeight = input.getBoundingClientRect().height;
      return { left: screenRect.left, cursorTop: input.getBoundingClientRect().top, cellWidth, cellHeight };
    });
    if (geometry.cellWidth <= 0 || geometry.cellHeight <= 0) return false;
    for (let row = 0; row < 4; row += 1) {
      for (let column = 0; column < 28; column += 1) {
        const point = {
          x: geometry.left + geometry.cellWidth * (column + 0.5),
          y: geometry.cursorTop - geometry.cellHeight * (row + 0.5),
          cellWidth: geometry.cellWidth,
        };
        await page.mouse.move(point.x, point.y);
        const hint = page.locator(".terminal-pm-link-hint");
        if (await page.locator(".xterm-cursor-pointer").count() === 1
          && await hint.count() === 1
          && (await hint.textContent())?.includes(expectedHref)) {
          found = point;
          return true;
        }
      }
    }
    return false;
  }, { message: `rendered terminal link ${expectedHref}`, timeout: 10_000 }).toBe(true);
  return found!;
}

async function expectLinkHover(page: Page, point: { x: number; y: number }): Promise<void> {
  await page.mouse.move(point.x, point.y);
  await expect(page.locator(".xterm-cursor-pointer")).toHaveCount(1);
}

async function modifiedClick(page: Page, point: { x: number; y: number }): Promise<void> {
  await page.keyboard.down("Shift");
  await page.mouse.click(point.x, point.y);
  await page.keyboard.up("Shift");
}

async function recordOpenedUrls(page: Page): Promise<void> {
  await page.evaluate(() => {
    const current = window as typeof window & { __terminalOpenedUrls?: string[] };
    current.__terminalOpenedUrls = [];
    window.open = ((url?: string | URL) => {
      if (url !== undefined) current.__terminalOpenedUrls!.push(url.toString());
      return null;
    }) as typeof window.open;
  });
}

async function openedUrls(page: Page): Promise<string[]> {
  return page.evaluate(() => (
    (window as typeof window & { __terminalOpenedUrls?: string[] }).__terminalOpenedUrls ?? []
  ));
}

async function clickWithModifier(
  page: Page,
  point: { x: number; y: number },
  modifier: "Meta" | "Control" | "Shift",
): Promise<void> {
  await page.keyboard.down(modifier);
  await page.mouse.click(point.x, point.y);
  await page.keyboard.up(modifier);
}

async function terminalLinkState(page: Page, reference?: string) {
  return page.evaluate((target) => {
    const stage = (window as LinkReturnWindow).__pmStage;
    const debug = stage?.debugSnapshot().find((candidate) => candidate.visible)
      ?? stage?.debugSnapshot()[0];
    const layer = debug ? stage?.layers.get(debug.key) : undefined;
    if (!debug || !layer) throw new Error("missing terminal layer");
    const buffer = layer.term.buffer.active;
    let referenceRow = -1;
    let referenceColumn = -1;
    if (target) {
      for (let row = buffer.baseY + layer.term.rows - 1; row >= 0; row -= 1) {
        const text = buffer.getLine(row)?.translateToString(true) ?? "";
        const column = text.indexOf(target);
        if (column < 0) continue;
        referenceRow = row;
        referenceColumn = column;
        break;
      }
    }
    return {
      key: debug.key,
      layerId: layer.el.dataset.linkReturnLayerId ??= crypto.randomUUID(),
      baseY: buffer.baseY,
      viewportY: buffer.viewportY,
      linesFromBottom: Math.max(0, buffer.baseY - buffer.viewportY),
      selection: layer.term.getSelection(),
      referenceRow,
      referenceColumn,
    };
  }, reference);
}

async function revealReference(page: Page, reference: string): Promise<{ x: number; y: number; cellWidth: number }> {
  await expect.poll(() => terminalLinkState(page, reference).then((state) => state.referenceRow))
    .toBeGreaterThanOrEqual(0);
  await page.evaluate((target) => {
    const stage = (window as LinkReturnWindow).__pmStage!;
    const debug = stage.debugSnapshot().find((candidate) => candidate.visible)!;
    const layer = stage.layers.get(debug.key)!;
    const buffer = layer.term.buffer.active;
    for (let row = buffer.baseY + layer.term.rows - 1; row >= 0; row -= 1) {
      if (!(buffer.getLine(row)?.translateToString(true) ?? "").includes(target)) continue;
      layer.term.scrollToLine(Math.max(0, row - Math.floor(layer.term.rows / 2)));
      return;
    }
    throw new Error(`missing ${target}`);
  }, reference);
  const state = await terminalLinkState(page, reference);
  const terminal = page.locator('.term-layer[style*="visible"] .xterm');
  const point = await terminal.evaluate((element, position) => {
    const screen = element.querySelector<HTMLElement>(".xterm-screen")!;
    const input = element.querySelector<HTMLTextAreaElement>(".xterm-helper-textarea")!;
    const bounds = screen.getBoundingClientRect();
    const cellWidth = input.getBoundingClientRect().width;
    const cellHeight = input.getBoundingClientRect().height;
    return {
      x: bounds.left + cellWidth * (position.referenceColumn + 0.5),
      y: bounds.top + cellHeight * (position.referenceRow - position.viewportY + 0.5),
      cellWidth,
    };
  }, state);
  await expect.poll(async () => {
    // xterm caches the last buffer cell, including across a retained route.
    // Cross a full cell boundary before re-entering the link.
    await page.mouse.move(point.x + point.cellWidth * 3, point.y);
    await page.mouse.move(point.x, point.y);
    return page.locator(".xterm-cursor-pointer").count();
  }, { message: `hover ${reference}` }).toBe(1);
  return point;
}

async function returnThroughSidebar(page: Page, sessionId: string): Promise<void> {
  await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title", { hasText: /^browser-e2e$/ }) }).click();
  await expect(page).toHaveURL(new RegExp(`#\\/session\\/${sessionId}$`));
}

test("item-link returns restore scroll and finish xterm selection gestures", async ({ page }) => {
  const itemOutput = cli([
    "items", "add", "--bucket", "1", "--project", "1", "--status", "planned",
    "Terminal return lifecycle target",
  ]);
  const itemId = itemOutput.match(/pm:item\/1\/(\d+) created/)?.[1];
  if (!itemId) throw new Error(`could not resolve browser test item id from: ${itemOutput}`);
  const reference = `pm:item/1/${itemId}`;

  await page.setViewportSize({ width: 720, height: 760 });
  await logIn(page, { minimumSessions: 2 });
  await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title", { hasText: /^browser-e2e$/ }) }).click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);
  const sessionId = page.url().match(/#\/session\/(\d+)$/)?.[1];
  if (!sessionId) throw new Error("seeded browser session did not open");
  const input = page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
  await input.focus();
  await input.pressSequentially("lineout 700");
  await input.press("Enter");
  await expect.poll(() => terminalLinkState(page).then((state) => state.baseY)).toBeGreaterThan(600);
  await input.pressSequentially(`echo ${reference}`);
  await input.press("Enter");
  const beforeTail = (await terminalLinkState(page)).baseY;
  await input.pressSequentially("lineout 100");
  await input.press("Enter");
  await expect.poll(() => terminalLinkState(page).then((state) => state.baseY))
    .toBeGreaterThan(beforeTail + 80);

  let point = await revealReference(page, reference);
  const initial = await terminalLinkState(page, reference);
  expect(initial.linesFromBottom).toBeGreaterThan(5);

  const paths = ["back", "sidebar", "back", "sidebar"] as const;
  for (const [index, path] of paths.entries()) {
    if (index > 0) point = await revealReference(page, reference);
    const departing = await terminalLinkState(page, reference);
    await page.mouse.click(point.x, point.y);
    await expect(page).toHaveURL(new RegExp(`#\\/bucket\\/1\\/item\\/${itemId}$`));
    // Exercise the stale document-level selection listeners that used to stay
    // installed after the link's mouseup hid the terminal layer.
    await page.mouse.move(710, 4);
    // e2e-real-time-wait: xterm's drag-scroll lifecycle ticks every 50ms
    await page.waitForTimeout(120);
    if (path === "back") {
      await page.goBack();
      await expect(page).toHaveURL(new RegExp(`#\\/session\\/${sessionId}$`));
    } else {
      await returnThroughSidebar(page, sessionId);
    }
    await expect(page.locator('.term-layer[style*="visible"]')).toBeVisible();
    await expect.poll(async () => (await terminalLinkState(page, reference)).linesFromBottom)
      .toBeLessThanOrEqual(departing.linesFromBottom + 2);
    const returned = await terminalLinkState(page, reference);
    expect(returned.layerId).toBe(initial.layerId);
    expect(Math.abs(returned.linesFromBottom - departing.linesFromBottom)).toBeLessThanOrEqual(2);
    expect(returned.selection).toBe("");
  }

  // A real drag remains a selection gesture and must not activate the link.
  point = await revealReference(page, reference);
  await page.mouse.move(point.x, point.y);
  await page.mouse.down();
  await page.mouse.move(point.x + point.cellWidth * 8, point.y, { steps: 4 });
  await page.mouse.up();
  await expect(page).toHaveURL(new RegExp(`#\\/session\\/${sessionId}$`));
  await expect.poll(() => terminalLinkState(page).then((state) => state.selection.length)).toBeGreaterThan(2);
});

test("PM links preserve selection and route from session and workspace terminals", async ({ page }) => {
  const itemOutput = cli([
    "items", "add", "--bucket", "1", "--project", "1", "--status", "planned",
    "Terminal plain-click target",
  ]);
  const itemId = itemOutput.match(/pm:item\/1\/(\d+) created/)?.[1];
  if (!itemId) throw new Error(`could not resolve browser test item id from: ${itemOutput}`);

  await logIn(page);
  const rows = page.locator(".sb-session");
  await rows.first().click();
  const sourceUrl = page.url();
  const sourceId = sourceUrl.match(/#\/session\/(\d+)$/)?.[1];
  await rows.nth(1).click();
  const targetId = page.url().match(/#\/session\/(\d+)$/)?.[1];
  if (!sourceId || !targetId) throw new Error("could not resolve browser test session ids");
  await rows.first().click();
  await expect(page).toHaveURL(new RegExp(`#\\/session\\/${sourceId}$`));

  const sessionLink = await writeReference(page, ".term-layer", `pm:session/${targetId}`);
  await expectLinkHover(page, sessionLink);
  await page.mouse.move(sessionLink.x, sessionLink.y);
  await page.mouse.down();
  await page.mouse.move(sessionLink.x + sessionLink.cellWidth * 8, sessionLink.y);
  await page.mouse.up();
  await expect(page).toHaveURL(new RegExp(`#\\/session\\/${sourceId}$`));

  await expectLinkHover(page, sessionLink);
  await expect(page.locator(".terminal-pm-link-hint")).toContainText(`Click to open pm:session/${targetId}`);
  await page.mouse.click(sessionLink.x, sessionLink.y);
  await expect(page).toHaveURL(new RegExp(`#\\/session\\/${targetId}$`));

  await rows.first().click();
  const itemLink = await writeReference(page, ".term-layer", `pm:item/1/${itemId}`);
  await expectLinkHover(page, itemLink);
  await page.mouse.click(itemLink.x, itemLink.y);
  await expect(page).toHaveURL(new RegExp(`#\\/bucket\\/1\\/item\\/${itemId}$`));

  await page.goto(`${process.env.PM_E2E_BASE_URL!}/#/session/${sourceId}`);
  const legacyItemLink = await writeReference(page, ".term-layer", `pm:item/${itemId}`);
  await expectLinkHover(page, legacyItemLink);
  await page.mouse.click(legacyItemLink.x, legacyItemLink.y);
  await expect(page.locator(".workspace-flash")).toContainText("unqualified");
  await expect(page).toHaveURL(new RegExp(`#\\/session\\/${sourceId}$`));

  await page.goto(`${process.env.PM_E2E_BASE_URL!}/#/session/${targetId}`);
  await expect(page).toHaveURL(new RegExp(`#\\/session\\/${targetId}$`));

  const staleLink = await writeReference(page, ".term-layer", "pm:session/999999");
  const missingAlert = page.getByRole("alert");
  await expectLinkHover(page, staleLink);
  await page.mouse.click(staleLink.x, staleLink.y);
  await expect(missingAlert).toContainText("Session 999999 is missing or inaccessible.");
  await expect(page).toHaveURL(new RegExp(`#\\/session\\/${targetId}$`));

  const terminalInput = page.locator(".term-layer .xterm").filter({ visible: true }).locator(".xterm-helper-textarea");
  await terminalInput.focus();
  await terminalInput.pressSequentially("mouseon");
  await terminalInput.press("Enter");
  const mouseModeLink = await writeReference(page, ".term-layer", `pm:session/${sourceId}`);
  await expectLinkHover(page, mouseModeLink);
  await expect(page.locator(".terminal-pm-link-hint")).toContainText("Shift-click (Option-click on macOS)");
  await page.mouse.click(mouseModeLink.x, mouseModeLink.y);
  await expect(page).toHaveURL(new RegExp(`#\\/session\\/${targetId}$`));
  await expectLinkHover(page, mouseModeLink);
  await modifiedClick(page, mouseModeLink);
  await expect(page).toHaveURL(new RegExp(`#\\/session\\/${sourceId}$`));

  await page.getByTitle("new workspace").click();
  await page.getByLabel("name", { exact: true }).fill("link workspace");
  await page.getByRole("button", { name: "create workspace" }).click();
  await page.getByLabel("terminal shown in pane").selectOption({ index: 2 });
  await expect(page.locator(".workspace-terminal-host .xterm")).toBeVisible();

  const workspaceLink = await writeReference(page, ".workspace-terminal-host", `pm:item/1/${itemId}`);
  await expectLinkHover(page, workspaceLink);
  await expect(page.locator(".terminal-pm-link-hint")).toContainText(`Click to open pm:item/1/${itemId}`);
  await page.mouse.click(workspaceLink.x, workspaceLink.y);
  await expect(page).toHaveURL(new RegExp(`#\\/bucket\\/1\\/item\\/${itemId}$`));
});

test("HTTP links open intentionally from session and workspace terminals", async ({ page }) => {
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await recordOpenedUrls(page);

  const sessionUrl = "https://example.test/session?q=one#result";
  const sessionLink = await writeReference(page, ".term-layer", sessionUrl);
  await expectLinkHover(page, sessionLink);
  await expect(page.locator(".terminal-pm-link-hint")).toContainText("Command-click, Ctrl-click, and Shift-click also work");

  await page.mouse.move(sessionLink.x, sessionLink.y);
  await page.mouse.down();
  await page.mouse.move(sessionLink.x + sessionLink.cellWidth * 8, sessionLink.y);
  await page.mouse.up();
  expect(await openedUrls(page), "dragging a URL must remain selection").toEqual([]);

  await expectLinkHover(page, sessionLink);
  await page.mouse.click(sessionLink.x, sessionLink.y);
  await expect.poll(() => openedUrls(page)).toEqual([sessionUrl]);

  for (const modifier of ["Meta", "Control", "Shift"] as const) {
    await expectLinkHover(page, sessionLink);
    await clickWithModifier(page, sessionLink, modifier);
  }
  await expect.poll(() => openedUrls(page)).toEqual([
    sessionUrl,
    sessionUrl,
    sessionUrl,
    sessionUrl,
  ]);

  const terminalInput = page.locator(".term-layer .xterm").filter({ visible: true }).locator(".xterm-helper-textarea");
  await terminalInput.focus();
  await terminalInput.pressSequentially("mouseon");
  await terminalInput.press("Enter");
  const trackedUrl = "http://example.test/mouse-tracking";
  const trackedLink = await writeReference(page, ".term-layer", trackedUrl);
  await expectLinkHover(page, trackedLink);
  await expect(page.locator(".terminal-pm-link-hint")).toContainText("Shift-click (Option-click on macOS)");
  await page.mouse.click(trackedLink.x, trackedLink.y);
  await expect.poll(() => openedUrls(page)).toHaveLength(4);
  await expectLinkHover(page, trackedLink);
  await clickWithModifier(page, trackedLink, "Shift");
  await expect.poll(() => openedUrls(page)).toEqual([
    sessionUrl,
    sessionUrl,
    sessionUrl,
    sessionUrl,
    trackedUrl,
  ]);

  await page.getByTitle("new workspace").click();
  await page.getByLabel("name", { exact: true }).fill("URL workspace");
  await page.getByRole("button", { name: "create workspace" }).click();
  await page.getByLabel("terminal shown in pane").selectOption({ index: 2 });
  await expect(page.locator(".workspace-terminal-host .xterm")).toBeVisible();

  const workspaceUrl = "https://example.test/saved-workspace";
  const workspaceLink = await writeReference(page, ".workspace-terminal-host", `${workspaceUrl}.`, workspaceUrl);
  await expectLinkHover(page, workspaceLink);
  await expect(page.locator(".terminal-pm-link-hint")).toContainText("Shift-click (Option-click on macOS)");
  const beforeWorkspaceClick = (await openedUrls(page)).length;
  await page.mouse.click(workspaceLink.x, workspaceLink.y);
  await expect.poll(async () => (await openedUrls(page)).length).toBe(beforeWorkspaceClick);
  await expectLinkHover(page, workspaceLink);
  await clickWithModifier(page, workspaceLink, "Shift");
  await expect.poll(() => openedUrls(page)).toContain(workspaceUrl);
  expect(await openedUrls(page)).not.toContain(`${workspaceUrl}.`);
});


test("an agent's HTTP review link opens only the fullscreen review and closes when finished", async ({ page, context, isolatedDaemon }) => {
  const session = isolatedDaemon.session("browser-e2e")!;
  const root = mkdtempSync(join(tmpdir(), "pm-review-link-"));
  const git = (...args: string[]) => execFileSync("git", args, {
    cwd: root,
    env: { ...process.env, GIT_CONFIG_GLOBAL: "/dev/null", GIT_CONFIG_SYSTEM: "/dev/null" },
  });
  git("init", "-q", "-b", "main");
  git("config", "user.email", "browser@localhost");
  git("config", "user.name", "browser test");
  writeFileSync(join(root, "a.txt"), "before\n");
  git("add", "a.txt");
  git("commit", "-q", "-m", "Base.");
  writeFileSync(join(root, "a.txt"), "after\n");
  const output = cli(["review", "open", String(session.id), "--worktree", root, "--label", "HTTP review"]);
  const reviewId = Number(output.match(/review (\d+) open/)![1]);
  await logIn(page);
  await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title", { hasText: /^browser-e2e$/ }) }).click();
  const sourceUrl = page.url();
  const href = `${process.env.PM_E2E_BASE_URL!}/#/review/${reviewId}`;
  const link = await writeReference(page, ".term-layer", href);
  const [opened] = await Promise.all([
    context.waitForEvent("page"),
    page.mouse.click(link.x, link.y),
  ]);
  await expect(opened.locator(".review-rail-file").first()).toBeVisible();
  await expect(opened.locator(".shell")).toHaveClass(/is-focus-mode/);
  await expect(opened.locator(".xterm, .sidebar, .topbar")).toHaveCount(0);
  await expect(page.locator(".terminal-size-prompt")).toBeHidden();
  await Promise.all([
    opened.waitForEvent("close"),
    opened.getByRole("button", { name: "Finish review" }).click(),
  ]);
  await expect(page).toHaveURL(sourceUrl);
});
