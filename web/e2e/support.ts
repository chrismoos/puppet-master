import { expect, type Locator, type Page } from "@playwright/test";

export const USERNAME = "browser-e2e";
export const PASSWORD = "browser-e2e-password";

const CLOCK_OFFSET_MS = 60_000;
export const SCROLL_RENDER_MS = 100;

export async function installScrollbarClock(page: Page): Promise<void> {
  await page.clock.install({ time: Date.now() - CLOCK_OFFSET_MS });
}

export async function logIn(page: Page, options: { minimumSessions?: number } = {}): Promise<void> {
  await page.goto(process.env.PM_E2E_BASE_URL!);
  const create = page.getByRole("button", { name: "create user" });
  const inputs = page.locator("input");
  // Counting before the app has painted reads zero of both forms and skips
  // authentication entirely, so wait for whichever entry point is offered.
  await expect(create.or(page.getByRole("button", { name: "log in" })).or(page.locator(".sb-session")).first())
    .toBeAttached();
  if (await create.count()) {
    await inputs.nth(0).fill(USERNAME);
    await inputs.nth(1).fill(PASSWORD);
    await inputs.nth(2).fill(PASSWORD);
    await create.click();
  } else if (await page.getByRole("button", { name: "log in" }).count()) {
    await inputs.nth(0).fill(USERNAME);
    await inputs.nth(1).fill(PASSWORD);
    await page.getByRole("button", { name: "log in" }).click();
  }

  const rows = page.locator(".sb-session");
  if (options.minimumSessions !== undefined) {
    await expect.poll(() => rows.count()).toBeGreaterThanOrEqual(options.minimumSessions);
  } else {
    await expect(rows).toHaveCount(2);
  }
}

/**
 * Opens one Settings page the way a user does: the gear in the top bar,
 * then the page's entry in the Settings nav.
 */
export async function openSettings(page: Page, label: string): Promise<void> {
  await page.getByRole("link", { name: "Settings", exact: true }).click();
  const nav = page.getByRole("navigation", { name: "Settings" });
  await nav.getByRole("link", { name: label }).click();
  await expect(nav.getByRole("link", { name: label })).toHaveAttribute("aria-current", "page");
}

/// A computed colour as numbers. Chromium serializes a color-mix() result
/// as color(srgb ...) rather than rgba(), so comparing the string couples
/// a test to how the value was written rather than to what it paints.
export async function computedColor(
  locator: Locator,
  property = "backgroundColor",
): Promise<{ r: number; g: number; b: number; a: number }> {
  const value = await locator.evaluate(
    (element, name) => getComputedStyle(element)[name as "backgroundColor"],
    property,
  );
  const parts = value.match(/[\d.]+/g)?.map(Number) ?? [];
  const srgb = value.startsWith("color(");
  return {
    r: Math.round(srgb ? parts[0] * 255 : parts[0]),
    g: Math.round(srgb ? parts[1] * 255 : parts[1]),
    b: Math.round(srgb ? parts[2] * 255 : parts[2]),
    a: parts.length > 3 ? Number(parts[3].toFixed(3)) : 1,
  };
}

/**
 * The dashboard's access token, for a spec calling the API directly.
 *
 * The signed-in page's cookie mints it and authenticates nothing else, so a
 * spec that reaches past the UI has to make the same exchange the app does.
 */
export async function accessToken(page: Page): Promise<string> {
  const base = process.env.PM_E2E_BASE_URL as string;
  // The mint refuses a request that states no origin, and an API request context
  // sets none of its own, so a spec states the one a page would.
  const minted = await page.request.post(`${base}/api/web/token`, {
    headers: { Origin: new URL(base).origin },
  });
  if (!minted.ok()) throw new Error(`could not mint an access token (${minted.status()})`);
  const { accessToken: token } = (await minted.json()) as { accessToken: string };
  return token;
}

/**
 * Headers for a spec calling a daemon route directly.
 *
 * The token because the route takes one, and the origin because an API request
 * context shares the browser's cookies, so a state-changing call arrives
 * carrying the session cookie and the daemon checks where it came from.
 */
export async function apiHeaders(page: Page): Promise<Record<string, string>> {
  return {
    Authorization: `Bearer ${await accessToken(page)}`,
    Origin: new URL(process.env.PM_E2E_BASE_URL as string).origin,
  };
}

const TERMINAL_REVEAL_TIMEOUT_MS = 20_000;

/**
 * A switch keeps the previous layer on screen and focused until the new one
 * paints, so typing before this resolves reaches the previous terminal.
 */
export async function expectTerminalRevealed(page: Page, keyOrKind: string): Promise<string> {
  let revealed = "";
  await expect.poll(async () => {
    revealed = await page.evaluate((wanted) => {
      const stage = (window as unknown as { __pmStage?: {
        debugSnapshot(): Array<{ key: string; visible: boolean; socket: { phase: string } | null }>;
        layers: Map<string, { el: HTMLElement }>;
      } }).__pmStage;
      const selected = stage?.debugSnapshot().find((entry) => entry.visible);
      const matches = wanted.endsWith(":") ? selected?.key.startsWith(wanted) : selected?.key === wanted;
      if (!selected || !matches || selected.socket?.phase !== "online") return "";
      return stage?.layers.get(selected.key)?.el.style.visibility === "visible" ? selected.key : "";
    }, keyOrKind);
    return revealed;
  }, {
    message: `waiting for terminal ${keyOrKind} to be on screen and online`,
    timeout: TERMINAL_REVEAL_TIMEOUT_MS,
  }).not.toBe("");
  return revealed;
}
