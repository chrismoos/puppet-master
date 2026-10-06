import { expect, test, type Page } from "./fixtures";
import { computedColor, logIn } from "./support";

const SCROLLBAR_IDLE_MS = 750;

function thumbColor(page: Page): Promise<string> {
  return page.locator(".sb-scroll").evaluate((element) =>
    getComputedStyle(element, "::-webkit-scrollbar-thumb").backgroundColor);
}

test("the top bar tallies every session and filters the sidebar to one state", async ({ page, isolatedDaemon }) => {
  await logIn(page);

  const tallies = page.locator(".topbar-tallies");
  const blocked = tallies.getByRole("button", { name: /blocked/ });

  // The baseline runs two sessions and nothing is waiting, so only the states
  // actually present are offered.
  await expect(tallies.getByRole("button")).toHaveCount(1);
  await expect(tallies).toContainText("running");
  await expect(blocked).toHaveCount(0);

  isolatedDaemon.hookSession("browser-e2e", "needs-input", "pick a rollout window");
  await expect(blocked).toBeVisible();
  await expect(blocked).toHaveText("1 blocked");
  await expect(blocked).toHaveAttribute("aria-pressed", "false");

  // Attention leads: a blocked session outranks a running one.
  const order = await tallies.evaluate((element) =>
    [...element.querySelectorAll(".sb-rollup-tally")].map((tally) => tally.className.split(" ")[1]));
  expect(order).toEqual(["c-needs-input", "c-working"]);

  await blocked.click();
  await expect(blocked).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator(".sb-session")).toHaveCount(1);
  await expect(page.locator(".sb-session")).toContainText("browser-e2e");

  // Clicking the pressed tally returns the full list.
  await blocked.click();
  await expect(blocked).toHaveAttribute("aria-pressed", "false");
  await expect(page.locator(".sb-session")).toHaveCount(2);

  // The tally is keyboard reachable and toggles from the keyboard.
  await blocked.focus();
  await page.keyboard.press("Enter");
  await expect(blocked).toHaveAttribute("aria-pressed", "true");
  await expect(page.locator(".sb-session")).toHaveCount(1);

  // A filter whose last session moves on is dropped rather than left showing
  // an empty sidebar.
  isolatedDaemon.hookSession("browser-e2e", "prompt-submitted");
  await expect(page.locator(".sb-session")).toHaveCount(2);
  await expect(page.locator(".topbar-tallies [aria-pressed=true]")).toHaveCount(0);
});

test("offline hosts collapse behind a disclosure that keeps them reachable", async ({ page, isolatedDaemon }) => {
  isolatedDaemon.seedOfflineHosts(["garage-box", "attic-box"]);
  await isolatedDaemon.restart();
  await logIn(page);

  const hosts = page.getByRole("region", { name: "workers" });
  const disclosure = hosts.getByRole("button", { name: /offline/ });

  // Every host is offline, so hiding them would leave an apparently empty row.
  await expect(disclosure).toHaveText("▾2 offline");
  await expect(disclosure).toHaveAttribute("aria-expanded", "true");
  await expect(hosts.locator(".worker-chip")).toHaveCount(2);
  await expect(hosts.locator(".worker-chip", { hasText: "garage-box" })).toBeVisible();

  // They stay reachable in both directions — an offline host is the one you
  // want to inspect.
  await disclosure.click();
  await expect(disclosure).toHaveAttribute("aria-expanded", "false");
  await expect(hosts.locator(".worker-chip")).toHaveCount(0);
  await disclosure.click();
  await expect(disclosure).toHaveAttribute("aria-expanded", "true");
  await expect(hosts.locator(".worker-chip")).toHaveCount(2);

  const overflow = await page.locator(".sidebar").evaluate((element) =>
    element.scrollWidth - element.clientWidth);
  expect(overflow).toBeLessThanOrEqual(0);
});

test("the sidebar scrollbar appears around scrolling without moving the list", async ({ page, isolatedDaemon }) => {
  isolatedDaemon.seedEndedSessions(40);
  await isolatedDaemon.restart();
  await page.addInitScript(() => localStorage.setItem("pm.showEnded", "false"));
  await logIn(page);
  await page.getByLabel("show ended").check();
  await expect(page.locator(".sb-session")).toHaveCount(42);

  const scroll = page.locator(".sb-scroll");

  // Without a reserved gutter the list reflows every time the bar appears,
  // which is a worse jitter than an always-visible bar.
  await expect(scroll).toHaveCSS("scrollbar-gutter", "stable");

  const idleThumb = await thumbColor(page);
  expect(idleThumb).toBe("rgba(0, 0, 0, 0)");
  await expect(scroll).not.toHaveClass(/is-sb-scrollbar-active/);

  const before = await page.locator(".sb-bucket").first().evaluate((element) =>
    element.getBoundingClientRect().left);

  await scroll.hover();
  await page.mouse.wheel(0, 120);
  await expect(scroll).toHaveClass(/is-sb-scrollbar-active/);
  expect(await thumbColor(page)).not.toBe("rgba(0, 0, 0, 0)");

  // Revealing the bar must not move the rows it sits beside.
  const during = await page.locator(".sb-bucket").first().evaluate((element) =>
    element.getBoundingClientRect().left);
  expect(during).toBeCloseTo(before, 0);

  // e2e-real-time-wait: the idle retraction is a wall-clock timer, and its duration is the contract under test.
  await page.waitForTimeout(SCROLLBAR_IDLE_MS + 250);
  await expect(scroll).not.toHaveClass(/is-sb-scrollbar-active/);
  expect(await thumbColor(page)).toBe("rgba(0, 0, 0, 0)");
});

test("the sidebar scrollbar stays up for the whole of a thumb drag", async ({ page, isolatedDaemon }) => {
  isolatedDaemon.seedEndedSessions(40);
  await isolatedDaemon.restart();
  await page.addInitScript(() => localStorage.setItem("pm.showEnded", "false"));
  await logIn(page);
  await page.getByLabel("show ended").check();
  await expect(page.locator(".sb-session")).toHaveCount(42);

  const scroll = page.locator(".sb-scroll");
  const box = (await scroll.boundingBox())!;
  const gutterX = box.x + (await scroll.evaluate((element) => element.clientWidth)) + 4;

  // Hovering the gutter finds the bar, which is otherwise invisible and so
  // impossible to aim at.
  await page.mouse.move(gutterX, box.y + 60);
  await expect(scroll).toHaveClass(/is-sb-scrollbar-active/);

  await page.mouse.down();
  await page.mouse.move(gutterX, box.y + 140, { steps: 6 });

  // e2e-real-time-wait: the defect being guarded against is the idle timer firing mid-drag, so the drag must outlast that real timer.
  await page.waitForTimeout(SCROLLBAR_IDLE_MS + 400);
  await expect(scroll).toHaveClass(/is-sb-scrollbar-active/);
  expect(await thumbColor(page)).not.toBe("rgba(0, 0, 0, 0)");

  await page.mouse.up();
  // e2e-real-time-wait: the release restarts the same wall-clock idle timer.
  await page.waitForTimeout(SCROLLBAR_IDLE_MS + 250);
  await expect(scroll).not.toHaveClass(/is-sb-scrollbar-active/);

  // Only a press on the bar holds the reveal open. A press on the rows beside
  // it is an ordinary click and must still let the bar retract, which is what
  // makes the hold above attributable to the scrollbar rather than to any
  // press at all.
  await page.mouse.move(box.x + 40, box.y + 60);
  await page.mouse.wheel(0, 60);
  await expect(scroll).toHaveClass(/is-sb-scrollbar-active/);
  await page.mouse.down();
  // e2e-real-time-wait: the same wall-clock idle timer must still expire under a press that never touched the bar.
  await page.waitForTimeout(SCROLLBAR_IDLE_MS + 250);
  await expect(scroll).not.toHaveClass(/is-sb-scrollbar-active/);
  await page.mouse.up();
});

test("a session on an offline host keeps its row, greyed and explained", async ({ page, isolatedDaemon }) => {
  isolatedDaemon.seedSessionOnOfflineHost("browser-e2e", "garage-box");
  await isolatedDaemon.restart();
  await logIn(page);

  // The complaint this covers is a row that vanished: work that still exists
  // must stay in the list while its host is away.
  await expect(page.locator(".sb-session")).toHaveCount(2);
  const stranded = page.locator(".sb-session-row").filter({
    has: page.locator(".sb-session-title", { hasText: "browser-e2e" }),
  }).filter({ has: page.locator(".state-dot.st-awaiting-worker") });
  await expect(stranded).toHaveCount(1);
  await expect(stranded.getByRole("button", { name: /awaiting worker/ })).toBeVisible();
  await expect(stranded.locator(".sb-session-detail"))
    .toHaveText("worker offline, resumes when it reconnects");

  // Grey, not red: the host is unreachable, which is neither a failure of the
  // session nor anything the reader did.
  const dot = await computedColor(stranded.locator(".state-dot"));
  expect(Math.max(dot.r, dot.g, dot.b) - Math.min(dot.r, dot.g, dot.b)).toBeLessThan(40);
  expect(dot.r).toBeLessThanOrEqual(dot.b);

  // And its own grey, so it does not read as one of the finished states.
  const others = await page.evaluate(() => {
    const read = (state: string) => {
      const probe = document.createElement("span");
      probe.className = `state-dot st-${state}`;
      document.body.append(probe);
      const color = getComputedStyle(probe).backgroundColor;
      probe.remove();
      return color;
    };
    return { failed: read("failed"), exited: read("exited") };
  });
  const rgb = `rgb(${dot.r}, ${dot.g}, ${dot.b})`;
  expect(others.failed).not.toBe(rgb);
  expect(others.exited).not.toBe(rgb);

  // The full badge, shown wherever a session gets one, carries the same
  // reading: a dashed edge where every settled state has a solid one.
  const badges = await page.evaluate(() => {
    const read = (state: string) => {
      const probe = document.createElement("span");
      probe.className = `badge st-${state}`;
      document.body.append(probe);
      const style = getComputedStyle(probe).borderStyle;
      probe.remove();
      return style;
    };
    return { awaiting: read("awaiting-worker"), exited: read("exited"), failed: read("failed") };
  });
  expect(badges.awaiting).toBe("dashed");
  expect(badges.exited).toBe("solid");
  expect(badges.failed).toBe("solid");

  // The title keeps its full-strength colour, the way a live row does, rather
  // than the dimmed one an ended session gets.
  const titles = await page.locator(".sb-session-title").evaluateAll((nodes) =>
    nodes.map((node) => getComputedStyle(node).color));
  expect(new Set(titles).size).toBe(1);

  // Hiding ended sessions must not take it with them.
  await page.getByLabel("show ended").uncheck();
  await expect(stranded).toHaveCount(1);
});
