import { expect, test } from "./fixtures";
import { logIn } from "./support";
const GPU_CONTEXT_LIMIT = 8;

interface TextureAtlas {
  pageLayoutVersion?: number;
  pages?: unknown[];
}

interface StageWindow extends Window {
  __previousAtlas?: TextureAtlas;
  __pmStage?: {
    layers: Map<string, {
      el: HTMLElement;
      webgl: { active: boolean };
      term: {
        _addonManager?: {
          _addons: Array<{
            instance?: {
              textureAtlas?: TextureAtlas;
            };
          }>;
        };
      };
    }>;
  };
}

test("switching terminals creates an independent texture atlas within the GPU context budget", async ({ page }) => {
  await logIn(page);
  const sessions = page.locator(".sb-session");
  await expect(sessions.nth(1)).toBeVisible();

  await sessions.nth(0).click();
  await expect(page.locator(".xterm")).toBeVisible();

  await expect.poll(() => page.evaluate(() => {
    const state = window as StageWindow;
    const layer = Array.from(state.__pmStage!.layers.values()).find((entry) => entry.el.style.visibility === "visible");
    const atlas = layer?.term._addonManager?._addons.find((entry) => entry.instance?.textureAtlas)?.instance?.textureAtlas;
    state.__previousAtlas = atlas;
    return Boolean(atlas);
  })).toBe(true);

  await sessions.nth(1).click();
  await expect(sessions.nth(1)).toHaveClass(/is-selected/);

  await expect.poll(() => page.evaluate((maximum) => {
    const state = window as StageWindow;
    const layers = Array.from(state.__pmStage!.layers.values());
    const visible = layers.find((layer) => layer.el.style.visibility === "visible");
    const current = visible?.term._addonManager?._addons.find((entry) => entry.instance?.textureAtlas)?.instance?.textureAtlas;
    return {
      active: Boolean(visible?.webgl.active),
      independent: Boolean(current && current !== state.__previousAtlas),
      withinBudget: layers.filter((layer) => layer.webgl.active).length <= maximum,
    };
  }, GPU_CONTEXT_LIMIT)).toEqual({ active: true, independent: true, withinBudget: true });
});
