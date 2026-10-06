import { execFileSync } from "node:child_process";
import { expect, test, type Page } from "./fixtures";
import { computedColor, logIn } from "./support";

/**
 * The nesting contract is CSS: a container query swaps the rollup summary, `:has()`
 * picks a child title colour from the state dot, and the connector geometry is
 * layout. Sidebar.test.ts owns the grouping, counts and filter/collapse logic;
 * this fixture is the markup Sidebar.tsx emits, rendered against the real
 * stylesheet.
 */

const child = (id: string, state: string, extra: string, title: string) => `
  <div class="sb-session-row sb-child ${extra}" data-child="${id}">
    <div class="sb-session-main">
      <button class="sb-session" type="button" aria-label="${title}, ${state}">
        <span class="sb-session-gutter"><span class="state-dot st-${state}" role="img" aria-label="${state}"></span></span>
        <span class="sb-session-content">
          <span class="sb-session-title">${title}</span>
          <span class="sb-session-detail">Claude is waiting for your input</span>
          <span class="sb-session-meta">
            <span class="sb-session-role is-worker" role="img" aria-label="Worker role"></span>
            <span class="sb-session-elapsed">3m</span>
          </span>
        </span>
      </button>
    </div>
    <button class="sb-menu-btn" type="button" aria-label="session menu">⋯</button>
  </div>`;

async function installFixture(page: Page): Promise<void> {
  const markup = `
      <aside class="sidebar sidebar-tree-fixture" style="width:304px">
        <div class="sb-scroll">
          <div class="sb-bucket">
            <div class="sb-supervisor is-open">
              <div class="sb-session-row" data-supervisor="2">
                <div class="sb-session-main">
                  <button class="sb-session" type="button" aria-label="Q&amp;A triage, working">
                    <span class="sb-session-gutter"><span class="state-dot st-working" role="img" aria-label="working"></span></span>
                    <span class="sb-session-content">
                      <span class="sb-session-title">Q&amp;A triage on the field-issue list</span>
                      <span class="sb-session-detail">Supervising six workers</span>
                      <span class="sb-session-meta">
                        <span class="sb-session-role is-supervisor" role="img" aria-label="Supervisor role"></span>
                        <span class="sb-session-elapsed is-now">now</span>
                      </span>
                    </span>
                  </button>
                </div>
                <button class="sb-menu-btn" type="button" aria-label="supervisor menu">⋯</button>
              </div>
              <div class="sb-rollup">
                <span class="sb-rollup-meter" role="img" aria-label="1 running, 2 blocked, 1 ready, 1 failed">
                  <i class="m-working" style="flex:1"></i><i class="m-needs-input" style="flex:2"></i><i class="m-idle" style="flex:1"></i><i class="m-failed" style="flex:1"></i>
                </span>
                <span class="sb-rollup-summary">
                  <span class="sb-rollup-summary-brief"><button type="button" class="sb-rollup-tally c-needs-input" aria-pressed="false"><b>2</b> need you</button><span class="sb-rollup-sep">·</span><span class="sb-rollup-quiet">5 workers</span></span>
                  <span class="sb-rollup-summary-full"><button type="button" class="sb-rollup-tally c-working" aria-pressed="false"><b>1</b> running</button><span class="sb-rollup-sep">·</span><button type="button" class="sb-rollup-tally c-needs-input" aria-pressed="false"><b>2</b> blocked</button><span class="sb-rollup-sep">·</span><button type="button" class="sb-rollup-tally c-idle" aria-pressed="false"><b>1</b> ready</button><span class="sb-rollup-sep">·</span><button type="button" class="sb-rollup-tally c-failed" aria-pressed="false"><b>1</b> failed</button></span>
                </span>
                <button type="button" class="sb-rollup-toggle" aria-expanded="true" aria-label="hide the 5 workers under Q&amp;A triage"><span aria-hidden="true">▾</span></button>
              </div>
              <div class="sb-children">
                ${child("working", "working", "", "Cluster gauge sweep rewrite with a deliberately long identifying label")}
                ${child("blocked", "needs-input", "is-needs-input is-attention-unseen", "VIN TP logging control byte change with 174 tests green")}
                ${child("viewed", "needs-input", "is-needs-input is-attention-seen", "LDW disable committed and verified on the target build")}
                ${child("ready", "idle", "", "DC charge range display fix committed")}
                ${child("failed", "failed", "", "Regression sweep on the firmware image crashed the harness")}
                ${child("stranded", "awaiting-worker", "", "Fleet telemetry backfill on the host that dropped off the network")}
              </div>
            </div>
            <div class="sb-loose">
              <div class="sb-session-row is-needs-input is-attention-unseen" data-toplevel="loose">
                <div class="sb-session-main">
                  <button class="sb-session is-needs-input" type="button" aria-label="Unattached worker, needs input">
                    <span class="sb-session-gutter"><span class="state-dot st-needs-input" role="img" aria-label="needs input"></span></span>
                    <span class="sb-session-content">
                      <span class="sb-session-title">Unattached worker awaiting a decision</span>
                      <span class="sb-session-meta"><span class="sb-session-elapsed">11m</span></span>
                    </span>
                  </button>
                </div>
              </div>
            </div>
          </div>
        </div>
      </aside>`;
  await page.locator("#root").evaluate((root, html) => { root.innerHTML = html; }, markup);
}

async function childTitleColors(page: Page): Promise<Record<string, string>> {
  return page.locator(".sidebar-tree-fixture").evaluate((sidebar) => {
    const colors: Record<string, string> = {};
    for (const row of sidebar.querySelectorAll<HTMLElement>(".sb-child")) {
      const key = row.dataset.child!;
      colors[key] = getComputedStyle(row.querySelector<HTMLElement>(".sb-session-title")!).color;
    }
    return colors;
  });
}

const channels = (color: string) => color.match(/[\d.]+/g)!.slice(0, 3).map(Number);

test("a supervisor's workers hang off a connector rail without stealing top-level urgency", async ({ page }) => {
  await logIn(page);
  await installFixture(page);

  const sidebar = page.locator(".sidebar-tree-fixture");
  const rail = await sidebar.locator(".sb-children").evaluate((element) => ({
    width: getComputedStyle(element, "::before").width,
    left: getComputedStyle(element, "::before").left,
  }));
  expect(rail.width).toBe("1px");
  expect(rail.left).toBe("16px");

  const geometry = await sidebar.locator('.sb-child[data-child="working"]').evaluate((row) => {
    const gutter = row.querySelector<HTMLElement>(".sb-session-gutter")!;
    const dot = row.querySelector<HTMLElement>(".state-dot")!.getBoundingClientRect();
    return {
      elbow: getComputedStyle(gutter, "::before").width,
      dotWidth: dot.width,
      dotHeight: dot.height,
      dotLeft: dot.left - row.getBoundingClientRect().left,
      titleLeft: row.querySelector<HTMLElement>(".sb-session-title")!.getBoundingClientRect().left
        - row.getBoundingClientRect().left,
    };
  });
  expect(geometry.elbow).toBe("13px");
  expect(geometry.dotWidth).toBe(5);
  expect(geometry.dotHeight).toBe(5);
  expect(geometry.dotLeft).toBe(30);
  expect(geometry.titleLeft).toBe(40);

  const parentTitleLeft = await sidebar.locator('[data-supervisor="2"] .sb-session-title')
    .evaluate((title) => title.getBoundingClientRect().left);
  const childTitleLeft = await sidebar.locator('.sb-child[data-child="working"] .sb-session-title')
    .evaluate((title) => title.getBoundingClientRect().left);
  expect(childTitleLeft).toBeGreaterThan(parentTitleLeft);

  const blockedChild = sidebar.locator('.sb-child[data-child="blocked"]');
  await expect(blockedChild).toHaveCSS("background-color", "rgba(0, 0, 0, 0)");
  expect(await blockedChild.evaluate((row) => getComputedStyle(row, "::before").backgroundColor))
    .toBe("rgba(0, 0, 0, 0)");

  const topLevel = sidebar.locator('[data-toplevel="loose"]');
  expect(await computedColor(topLevel)).toEqual({ r: 255, g: 178, b: 36, a: 0.075 });
  expect(await topLevel.evaluate((row) => getComputedStyle(row, "::before").backgroundColor))
    .toBe("rgb(255, 178, 36)");

  await expect(sidebar.locator(".sb-child .sb-session-detail").first()).toBeHidden();
  await expect(sidebar.locator('[data-supervisor="2"] .sb-session-detail')).toBeHidden();

  for (const width of [264, 304, 420]) {
    await sidebar.evaluate((element, value) => {
      (element as HTMLElement).style.width = `${value}px`;
    }, width);
    const overflow = await sidebar.locator(".sb-session-row").evaluateAll((rows) =>
      rows.filter((row) => row.scrollWidth > row.clientWidth + 1).length);
    expect(overflow).toBe(0);
  }
});

test("a nested worker's state reads from its title colour", async ({ page }) => {
  await logIn(page);
  await installFixture(page);

  const colors = await childTitleColors(page);
  expect(new Set(Object.values(colors)).size).toBe(5);

  const [blockedR, blockedG, blockedB] = channels(colors.blocked);
  expect(blockedR).toBeGreaterThan(blockedG);
  expect(blockedG).toBeGreaterThan(blockedB);

  const viewed = channels(colors.viewed);
  expect(viewed[0]).toBeGreaterThan(viewed[2]);
  expect(viewed[0]).toBeLessThan(blockedR);

  const ready = channels(colors.ready);
  expect(ready[1]).toBeGreaterThan(ready[0]);
  expect(ready[1]).toBeGreaterThan(ready[2]);

  const failed = channels(colors.failed);
  expect(failed[0]).toBeGreaterThan(failed[1]);
  expect(failed[0]).toBeGreaterThan(failed[2]);

  // A child stranded on an offline host is still live work, so its title keeps
  // the neutral colour a running child gets rather than a finished one's.
  expect(colors.stranded).toBe(colors.working);
  expect(colors.stranded).not.toBe(colors.failed);
});

test("a child on an offline host is grey and says so, without reading as finished", async ({ page }) => {
  await logIn(page);
  await installFixture(page);

  const sidebar = page.locator(".sidebar-tree-fixture");
  const dots = await sidebar.locator(".sb-child").evaluateAll((rows) =>
    Object.fromEntries(rows.map((row) => [
      (row as HTMLElement).dataset.child!,
      getComputedStyle(row.querySelector<HTMLElement>(".state-dot")!).backgroundColor,
    ])));

  const [r, g, b] = channels(dots.stranded);
  expect(Math.max(r, g, b) - Math.min(r, g, b)).toBeLessThan(40);
  expect(r).toBeLessThanOrEqual(b);
  expect(dots.stranded).not.toBe(dots.failed);
  expect(dots.stranded).not.toBe(dots.blocked);

  await expect(sidebar.locator('.sb-child[data-child="stranded"] .state-dot'))
    .toHaveAttribute("aria-label", "awaiting-worker");
});

test("a resting worker is one line and the selected one gets the rest of the sentence", async ({ page }) => {
  await logIn(page);
  await installFixture(page);

  const sidebar = page.locator(".sidebar-tree-fixture");
  const resting = await sidebar.locator('.sb-child[data-child="working"] .sb-session-title')
    .evaluate((title) => ({
      clamp: getComputedStyle(title).webkitLineClamp,
      height: title.getBoundingClientRect().height,
      lineHeight: Number.parseFloat(getComputedStyle(title).lineHeight),
    }));
  expect(resting.clamp).toBe("1");
  expect(resting.height).toBeLessThan(resting.lineHeight * 2);

  await sidebar.locator('.sb-child[data-child="working"]')
    .evaluate((row) => row.classList.add("is-selected"));
  const selected = await sidebar.locator('.sb-child[data-child="working"] .sb-session-title')
    .evaluate((title) => ({
      clamp: getComputedStyle(title).webkitLineClamp,
      weight: getComputedStyle(title).fontWeight,
      height: title.getBoundingClientRect().height,
    }));
  expect(selected.clamp).toBe("2");
  expect(selected.weight).toBe("700");
  expect(selected.height).toBeGreaterThan(resting.height);
});

test("the rollup summary expands to the per-state breakdown only once the sidebar is wide", async ({ page }) => {
  await logIn(page);
  await installFixture(page);

  const sidebar = page.locator(".sidebar-tree-fixture");
  const brief = sidebar.locator(".sb-rollup-summary-brief");
  const full = sidebar.locator(".sb-rollup-summary-full");

  await expect(brief).toBeVisible();
  await expect(brief).toContainText("2 need you");
  await expect(brief).toContainText("5 workers");
  await expect(full).toBeHidden();

  await sidebar.evaluate((element) => { (element as HTMLElement).style.width = "420px"; });
  await expect(full).toBeVisible();
  await expect(full).toContainText("2 blocked");
  await expect(brief).toBeHidden();

  await sidebar.evaluate((element) => { (element as HTMLElement).style.width = "304px"; });
  await expect(brief).toBeVisible();
  await expect(full).toBeHidden();
});

test("the live sidebar files spawned sessions under their supervisor and nowhere else", async ({ page, isolatedDaemon }) => {
  const cli = (args: string[]) => execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
  cli(["spawn", "--project", "1", "--agent", "codex", "--title", "tree-boss", "--role", "supervisor"]);
  cli(["spawn", "--project", "1", "--agent", "codex", "--title", "tree-worker-blocked"]);
  cli(["spawn", "--project", "1", "--agent", "codex", "--title", "tree-worker-running"]);
  isolatedDaemon.setSessionParent("tree-worker-blocked", "tree-boss");
  isolatedDaemon.setSessionParent("tree-worker-running", "tree-boss");
  await isolatedDaemon.restart();

  await logIn(page, { minimumSessions: 5 });

  const supervisor = page.locator(".sb-supervisor").filter({ hasText: "tree-boss" });
  await expect(supervisor.locator(".sb-child")).toHaveCount(2);

  // A restart recovers every session as idle, so the blocked one is hooked live.
  isolatedDaemon.hookSession("tree-worker-blocked", "needs-input", "Choose the rollout window");
  await expect(supervisor.locator(".sb-child.is-needs-input")).toHaveCount(1);
  await expect(supervisor.locator(".sb-child .sb-session-title")).toHaveText([
    "tree-worker-blocked",
    "tree-worker-running",
  ]);
  // Every spawned worker is nested under its supervisor, none left at bucket level.
  await expect(page.locator(".sb-bucket > .sb-session-row .sb-session-title", { hasText: "tree-worker-" })).toHaveCount(0);

  const rollup = supervisor.locator(".sb-rollup");
  await expect(rollup.locator(".sb-rollup-summary-brief")).toContainText("1 need you");
  await expect(rollup.locator(".sb-rollup-summary-brief")).toContainText("2 workers");

  const caret = rollup.locator(".sb-rollup-toggle");
  await caret.click();
  await expect(caret).toHaveAttribute("aria-expanded", "false");
  await expect(supervisor.locator(".sb-child")).toHaveCount(0);
  await caret.click();
  await expect(supervisor.locator(".sb-child")).toHaveCount(2);

  const tally = rollup.locator(".sb-rollup-tally").first();
  await tally.click();
  await expect(tally).toHaveAttribute("aria-pressed", "true");
  await expect(supervisor.locator(".sb-child .sb-session-title")).toHaveText(["tree-worker-blocked"]);
  await expect(supervisor.locator(".sb-children-showall")).toContainText("showing 1 of 2 · show all");

  await supervisor.locator(".sb-children-showall").click();
  await expect(supervisor.locator(".sb-child")).toHaveCount(2);
  await expect(tally).toHaveAttribute("aria-pressed", "false");

  // The collapse and the filter are preferences, so they survive a reload.
  await tally.click();
  await page.reload();
  await expect(page.locator(".sb-supervisor").filter({ hasText: "tree-boss" }).locator(".sb-child"))
    .toHaveCount(1);
});

/**
 * A worker row shows its branch and its last-activity time in a box that is
 * positioned out of the row's flow. Anything that wraps there leaves the row
 * and crosses the divider beneath it, and with no column gap the two read as
 * one string.
 */
test("a worker's branch and activity time stay one readable line inside the row", async ({ page }) => {
  await logIn(page);
  await page.locator("#root").evaluate((root) => {
    root.innerHTML = `
      <aside class="sidebar sidebar-branch-fixture" style="width:304px">
        <div class="sb-scroll"><div class="sb-bucket"><div class="sb-children">
          <div class="sb-session-row sb-child" data-child="branchy">
            <div class="sb-session-main">
              <button class="sb-session" type="button" aria-label="worker">
                <span class="sb-session-gutter"><span class="state-dot st-working"></span></span>
                <span class="sb-session-content">
                  <span class="sb-session-title">Worker with a long branch name</span>
                  <span class="sb-session-meta">
                    <span class="sb-session-branch"><span class="sb-session-branch-name">item-232-recover-a-lost-device-id-instead-of-re-enrolling</span></span>
                    <span class="sb-session-elapsed">14m</span>
                  </span>
                </span>
              </button>
            </div>
          </div>
        </div></div></div>
      </aside>`;
  });

  const metrics = await page.locator(".sidebar-branch-fixture .sb-child").evaluate((row) => {
    const meta = row.querySelector<HTMLElement>(".sb-session-meta")!;
    const branch = row.querySelector<HTMLElement>(".sb-session-branch")!;
    // The ellipsis is on the inner name span; the outer branch sizes to
    // its content, so truncation is only visible on the name.
    const name = row.querySelector<HTMLElement>(".sb-session-branch-name")!;
    const elapsed = row.querySelector<HTMLElement>(".sb-session-elapsed")!;
    const metaBox = meta.getBoundingClientRect();
    return {
      metaHeight: metaBox.height,
      lineHeight: branch.getBoundingClientRect().height,
      metaBottom: metaBox.bottom,
      rowBottom: row.getBoundingClientRect().bottom,
      gap: elapsed.getBoundingClientRect().left - branch.getBoundingClientRect().right,
      branchTruncated: name.scrollWidth > name.clientWidth,
    };
  });

  // The branch is long enough that it has to truncate; that is the point.
  expect(metrics.branchTruncated).toBe(true);
  // One line, not two: a second line would sit outside the positioned box.
  expect(metrics.metaHeight).toBeLessThan(metrics.lineHeight * 1.6);
  expect(metrics.metaBottom).toBeLessThanOrEqual(metrics.rowBottom);
  // Enough space that the branch and the time do not read as one token.
  expect(metrics.gap).toBeGreaterThanOrEqual(4);
});

/**
 * The row colour and its accessible name both come from a state derived at
 * render time, so this needs the live daemon moving a real worker through
 * working and needs-input under an untouched idle supervisor.
 */
test("an idle supervisor reads as running only while a worker it spawned is going", async ({ page, isolatedDaemon }) => {
  const cli = (args: string[]) => execFileSync(process.env.PM_E2E_PM_BIN!, args, {
    env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET },
    encoding: "utf8",
  });
  cli(["spawn", "--project", "1", "--agent", "codex", "--title", "wait-boss", "--role", "supervisor"]);
  cli(["spawn", "--project", "1", "--agent", "codex", "--title", "wait-worker"]);
  isolatedDaemon.setSessionParent("wait-worker", "wait-boss");
  await isolatedDaemon.restart();

  await logIn(page, { minimumSessions: 4 });

  const supervisor = page.locator(".sb-supervisor").filter({ hasText: "wait-boss" });
  const row = supervisor.locator(".sb-session-row:not(.sb-child)");
  const dot = row.locator(".state-dot");
  const brief = supervisor.locator(".sb-rollup-summary-brief");

  // A restart recovers every session as idle, so the supervisor starts out
  // genuinely out of work rather than waiting.
  await expect(dot).toHaveClass(/st-idle/);
  await expect(brief).toContainText("1 worker, all quiet");

  isolatedDaemon.hookSession("wait-worker", "prompt-submitted");
  await expect(dot).toHaveClass(/st-working/);
  await expect(dot).toHaveAttribute("aria-label", "working");
  await expect(row.locator(".sb-session")).toHaveAttribute("aria-label", /, working$/);
  await expect(brief).toContainText("1 running");
  await expect(brief).not.toContainText("all quiet");

  // A worker that stops to ask a question is not the supervisor's own
  // question, so the row goes back to green and the rollup says who needs you.
  isolatedDaemon.hookSession("wait-worker", "needs-input", "Choose the rollout window");
  await expect(dot).toHaveClass(/st-idle/);
  await expect(row.locator(".sb-session")).toHaveAttribute("aria-label", /, idle$/);
  await expect(brief).toContainText("1 need you");
});
