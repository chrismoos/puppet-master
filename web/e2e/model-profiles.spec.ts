import { expect, test } from "./fixtures";
import { logIn } from "./support";

const API_KEY = "sk-browser-e2e-secret";
const NEW_PROFILE_MAX_WIDTH_PX = 560;

test("a model profile is configured, attached, and previewed before a spawn", async ({
  page,
  isolatedDaemon: _isolatedDaemon,
}) => {
  await logIn(page);
  await page.goto(`${process.env.PM_E2E_BASE_URL!}#/settings/models`);

  const panel = page.locator(".model-profiles");
  await expect(panel).toBeVisible();
  // Nothing configured reads as a real empty state, with the create form
  // kept to a narrow card under it.
  await expect(panel.locator(".ui-empty")).toContainText("No model profiles yet");
  const create = panel.getByRole("form", { name: "New profile" });
  expect((await create.boundingBox())!.width).toBeLessThanOrEqual(NEW_PROFILE_MAX_WIDTH_PX);
  await expect(create.getByRole("button", { name: "Create profile" })).toBeDisabled();
  await panel.getByLabel("Name", { exact: true }).fill("Gateway");
  await panel.getByLabel("API key", { exact: true }).fill(API_KEY);
  await panel.getByRole("button", { name: "Create profile" }).click();

  const profile = page.locator(".model-profile").first();
  await expect(profile).toBeVisible();
  await expect(panel.locator(".ui-empty")).toHaveCount(0);
  await expect(profile).toContainText("covers no agent yet");
  await expect(profile).toContainText("API key set");

  await profile.locator(".catalog-name").click();
  const anthropic = profile.locator('.model-endpoint[data-dialect="anthropic-messages"]');
  await anthropic.getByLabel("model", { exact: true }).fill("gateway/big");
  await anthropic.getByLabel("base URL").fill("https://gateway.invalid/v1");
  await anthropic.getByLabel("background model").fill("gateway/small");
  await anthropic.getByRole("button", { name: "Add endpoint" }).click();
  await expect(profile).toContainText("covers Claude, OpenCode");

  // Only dialects an adapter speaks are offered, so no entry can be
  // built that nothing selects.
  await expect(profile.locator(".model-endpoint")).toHaveCount(3);
  await expect(profile.locator('.model-endpoint[data-dialect="openai-chat"]')).toHaveCount(0);

  // Neither agent that runs this dialect has a small/fast model setting,
  // so it says so instead of taking a value the adapters drop.
  const responsesFieldset = profile.locator('.model-endpoint[data-dialect="openai-responses"]');
  await expect(responsesFieldset.getByTestId("background-na-openai-responses"))
    .toContainText("Codex, OpenCode have no small/fast model setting");
  await expect(responsesFieldset.getByLabel("background model")).toBeDisabled();
  await expect(anthropic.getByLabel("background model")).toBeEnabled();

  // Gemini speaks neither of the other dialects, so its own is what makes
  // the profile reach it at all, and it has no small/fast model setting.
  const genaiFieldset = profile.locator('.model-endpoint[data-dialect="google-genai"]');
  await expect(genaiFieldset.getByTestId("background-na-google-genai"))
    .toContainText("Gemini has no small/fast model setting");
  await expect(genaiFieldset.getByLabel("background model")).toBeDisabled();

  // The key is write-only: it is never rendered back into the page.
  await expect(page.locator("body")).not.toContainText(API_KEY);
  const keyField = profile.getByLabel("replace API key");
  await expect(keyField).toHaveValue("");
  await expect(keyField).toHaveAttribute("placeholder", /key set/);

  await page.goto(`${process.env.PM_E2E_BASE_URL!}#/settings/projects?catalog=buckets`);
  const bucket = page.locator(".manage-bucket").first();
  await bucket.locator(".catalog-name").click();
  const drawer = page.locator(".catalog-drawer");
  await drawer.getByLabel("Default Agent").selectOption("codex");
  await drawer.getByLabel("Model Profile").selectOption({ index: 1 });
  await drawer.getByRole("button", { name: "Save changes" }).click();
  // Codex speaks a dialect this profile does not cover yet, so attaching
  // it against the bucket's resolved agent is refused.
  await expect(page.locator(".catalog-error")).toContainText("no endpoint the codex agent can use");

  // The refused save already applied the agent change, so the drawer
  // reloads from the bucket and the profile has to be picked again.
  await expect(bucket.locator('[data-label="Default Agent"]')).toHaveText("Codex");
  await expect(drawer.getByLabel("Model Profile")).toHaveValue("");
  await drawer.getByLabel("Default Agent").selectOption("claude");
  await drawer.getByLabel("Model Profile").selectOption({ index: 1 });
  await drawer.getByRole("button", { name: "Save changes" }).click();
  await expect(drawer).toBeHidden();
  await expect(page.locator(".manage-bucket").first()).toContainText("Gateway");

  const projectName = await openSpawnDialogForFirstProject(page);
  const modal = page.locator(".modal");
  await expect(modal.getByTestId("effective-model-profile"))
    .toHaveText("Effective: Gateway · gateway/big · bucket default");
  await modal.getByLabel("agent").selectOption("codex");
  await expect(modal.getByTestId("effective-model-profile"))
    .toHaveText("Gateway has no endpoint Codex can use — this spawn will be rejected");
  await page.keyboard.press("Escape");

  await page.goto(`${process.env.PM_E2E_BASE_URL!}#/settings/models`);
  await page.locator(".model-profile").first().locator(".catalog-name").click();
  const responses = page.locator('.model-endpoint[data-dialect="openai-responses"]');
  await responses.getByLabel("model", { exact: true }).fill("gateway/o");
  await responses.getByLabel("base URL").fill("https://gateway.invalid/openai");
  await responses.getByRole("button", { name: "Add endpoint" }).click();
  await expect(page.locator(".model-profile").first()).toContainText("covers Claude, Codex, OpenCode");

  await openSpawnDialogForFirstProject(page, projectName);
  const spawnModal = page.locator(".modal");
  await spawnModal.getByLabel("agent").selectOption("codex");
  await expect(spawnModal.getByTestId("effective-model-profile"))
    .toHaveText("Effective: Gateway · gateway/o · bucket default");
  // Gemini is offered, and the profile cannot serve it until it carries a
  // google-genai entry.
  await spawnModal.getByLabel("agent").selectOption("gemini");
  await expect(spawnModal.getByTestId("effective-model-profile"))
    .toHaveText("Gateway has no endpoint Gemini can use — this spawn will be rejected");
  await page.keyboard.press("Escape");

  await page.goto(`${process.env.PM_E2E_BASE_URL!}#/settings/models`);
  await page.locator(".model-profile").first().locator(".catalog-name").click();
  const genai = page.locator('.model-endpoint[data-dialect="google-genai"]');
  await genai.getByLabel("model", { exact: true }).fill("gateway/gem");
  await genai.getByLabel("base URL").fill("https://gateway.invalid/genai");
  await genai.getByRole("button", { name: "Add endpoint" }).click();
  await expect(page.locator(".model-profile").first())
    .toContainText("covers Claude, Codex, Gemini, OpenCode");

  await openSpawnDialogForFirstProject(page, projectName);
  await page.locator(".modal").getByLabel("agent").selectOption("gemini");
  await expect(page.locator(".modal").getByTestId("effective-model-profile"))
    .toHaveText("Effective: Gateway · gateway/gem · bucket default");
  await page.keyboard.press("Escape");

  // A referenced profile cannot be deleted, and the error names the referent.
  await page.goto(`${process.env.PM_E2E_BASE_URL!}#/settings/models`);
  await page.locator(".model-profile").first().getByRole("button", { name: "Delete" }).click();
  await expect(page.locator(".model-profiles .flash-error")).toContainText("is still used by bucket");
});

async function openSpawnDialogForFirstProject(page: Parameters<typeof logIn>[0], known?: string) {
  let projectName = known;
  if (projectName === undefined) {
    await page.goto(`${process.env.PM_E2E_BASE_URL!}#/settings/projects?catalog=projects`);
    projectName = (await page.locator(".manage-project").first().locator(".catalog-name b").textContent())!;
  }
  await page.getByRole("button", { name: "Back to sessions" }).click();
  // The ＋ opens the quick popover; the full dialog stays on the bucket menu.
  await page.locator(".sb-bucket-row").first().getByTitle(/actions for /).click();
  await page.getByRole("menuitem", { name: "new session…" }).click();
  await expect(page.locator(".modal")).toBeVisible();
  return projectName;
}

test("the spawn modal stays reachable with a model profile field present", async ({
  page,
  isolatedDaemon: _isolatedDaemon,
}) => {
  await logIn(page);

  // The tallest state: a profile attached, an agent it cannot serve, and
  // the rejection warning wrapping on the narrowest supported width.
  await page.goto(`${process.env.PM_E2E_BASE_URL!}#/settings/models`);
  const panel = page.locator(".model-profiles");
  await panel.getByLabel("Name", { exact: true }).fill("A deliberately long gateway profile name");
  await panel.getByLabel("API key", { exact: true }).fill(API_KEY);
  await panel.getByRole("button", { name: "Create profile" }).click();
  const profile = page.locator(".model-profile").first();
  await profile.locator(".catalog-name").click();
  const anthropic = profile.locator('.model-endpoint[data-dialect="anthropic-messages"]');
  await anthropic.getByLabel("model", { exact: true }).fill("gateway/big");
  await anthropic.getByLabel("base URL").fill("https://gateway.invalid/v1");
  await anthropic.getByRole("button", { name: "Add endpoint" }).click();

  await page.goto(`${process.env.PM_E2E_BASE_URL!}#/settings/projects?catalog=buckets`);
  const bucket = page.locator(".manage-bucket").first();
  await bucket.locator(".catalog-name").click();
  const drawer = page.locator(".catalog-drawer");
  await drawer.getByLabel("Model Profile").selectOption({ index: 1 });
  await drawer.getByRole("button", { name: "Save changes" }).click();
  await expect(drawer).toBeHidden();

  await page.goto(`${process.env.PM_E2E_BASE_URL!}#/settings/projects?catalog=projects`);
  const projectName =
    (await page.locator(".manage-project").first().locator(".catalog-name b").textContent())!;
  await page.getByRole("button", { name: "Back to sessions" }).click();
  await page.locator(".sb-bucket-row").first().getByTitle(/actions for /).click();
  await page.getByRole("menuitem", { name: "new session…" }).click();

  const modal = page.locator(".modal");
  await expect(modal).toBeVisible();
  await modal.getByLabel("agent").selectOption("codex");
  await expect(modal.getByTestId("effective-model-profile")).toContainText("will be rejected");

  for (const viewport of [{ width: 1440, height: 1000 }, { width: 360, height: 800 }]) {
    await page.setViewportSize(viewport);
    const metrics = await modal.evaluate((element) => {
      const rect = element.getBoundingClientRect();
      return {
        top: rect.top,
        bottom: rect.bottom,
        viewportHeight: document.documentElement.clientHeight,
      };
    });
    expect(metrics.top).toBeGreaterThanOrEqual(0);
    expect(metrics.bottom).toBeLessThanOrEqual(metrics.viewportHeight);
  }

  // The actions row is the thing that went out of reach before, so click
  // it rather than merely asserting on its box.
  await modal.getByRole("button", { name: "cancel" }).click();
  await expect(modal).toBeHidden();
});
