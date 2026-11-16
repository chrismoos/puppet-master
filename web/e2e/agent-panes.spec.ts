import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { expect, test } from "./fixtures";
import { logIn } from "./support";

const SESSION = "browser-e2e";
const COLLAPSED_HEIGHT = 24;
const DRAG_PX = 120;

async function openSession(page: import("@playwright/test").Page) {
  await page
    .locator(".sb-session")
    .filter({ has: page.locator(".sb-session-title").getByText(SESSION, { exact: true }) })
    .click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);
}

function layoutOf(text: string): { rev: number; layout: unknown; unplaced: string[]; panes: Array<{ id: string; state: string }> } {
  return JSON.parse(text);
}

test("an agent's panes sit beside it and the user resizes, collapses, hides, restores and closes them", async ({ page, isolatedDaemon }) => {
  await logIn(page);
  await openSession(page);
  const cwd = isolatedDaemon.session(SESSION)!.cwd;
  await mkdir(join(cwd, "status"), { recursive: true });
  await writeFile(join(cwd, "status", "index.html"), "<!doctype html><h1>Status page</h1>");

  await isolatedDaemon.callAgentTool(SESSION, "open_page", { id: "status", title: "Build status", dir: "status" });
  const statusPane = page.locator(".ap-pane", { has: page.locator(".ap-title", { hasText: "Build status" }) });
  await expect(statusPane).toBeVisible();
  const frame = statusPane.locator("iframe");
  await expect(frame).toHaveAttribute("sandbox", "allow-scripts allow-forms");
  await expect(page.frameLocator(".ap-pane iframe").getByRole("heading", { name: "Status page" })).toBeVisible();

  isolatedDaemon.setSessionPermission(SESSION, "bypass");
  await isolatedDaemon.callAgentTool(SESSION, "open_terminal", { id: "job", title: "Echo job", command: "echo pane-ready; sleep 60" });
  const jobPane = page.locator(".ap-pane", { has: page.locator(".ap-title", { hasText: "Echo job" }) });
  await expect(jobPane.locator(".ap-status")).toHaveText("running");
  await expect.poll(async () => JSON.parse(await isolatedDaemon.callAgentTool(SESSION, "read_pane", { id: "job" })).text).toContain("pane-ready");
  await expect(page.locator(".terminal-tabs .terminal-tab", { hasText: "Echo job" })).toHaveCount(0);

  await jobPane.getByRole("button", { name: "Collapse pane" }).click();
  await expect(jobPane).toHaveClass(/is-collapsed/);
  await expect.poll(async () => (await jobPane.boundingBox())?.height).toBe(COLLAPSED_HEIGHT);
  await jobPane.getByRole("button", { name: "Expand pane" }).click();
  await expect(jobPane).not.toHaveClass(/is-collapsed/);

  const before = layoutOf(await isolatedDaemon.callAgentTool(SESSION, "get_layout", {}));
  const gutter = page.locator(".ap-gutter-row").first();
  const box = (await gutter.boundingBox())!;
  // A quarter of the way down, clear of where the stacked panes' divider meets this one.
  const grabY = box.y + box.height / 4;
  await page.mouse.move(box.x + box.width / 2, grabY);
  await page.mouse.down();
  await page.mouse.move(box.x - DRAG_PX / 2, grabY, { steps: 4 });
  await page.mouse.move(box.x - DRAG_PX, grabY, { steps: 4 });
  await page.mouse.up();
  await expect.poll(async () => layoutOf(await isolatedDaemon.callAgentTool(SESSION, "get_layout", {})).rev).toBeGreaterThan(before.rev);
  await expect.poll(async () => (await gutter.boundingBox())!.x).toBeLessThan(box.x - DRAG_PX / 2);

  await statusPane.getByRole("button", { name: "Hide" }).click();
  await expect(statusPane).toHaveCount(0);
  const chip = page.getByRole("button", { name: "↺ Build status" });
  await expect(chip).toBeVisible();
  expect(layoutOf(await isolatedDaemon.callAgentTool(SESSION, "get_layout", {})).unplaced).toEqual(["status"]);
  await chip.click();
  await expect(statusPane).toBeVisible();
  await expect(chip).toHaveCount(0);

  await jobPane.getByRole("button", { name: "Close" }).click();
  const dialog = page.getByRole("dialog", { name: "stop this program" });
  await expect(dialog).toBeVisible();
  const dialogBox = (await dialog.boundingBox())!;
  const viewport = page.viewportSize()!;
  expect(Math.abs(dialogBox.x + dialogBox.width / 2 - viewport.width / 2)).toBeLessThan(viewport.width / 10);
  await dialog.getByRole("button", { name: "stop" }).click();
  await expect(jobPane).toHaveCount(0);
  const after = layoutOf(await isolatedDaemon.callAgentTool(SESSION, "get_layout", {}));
  expect(after.panes.map((pane) => pane.id)).toEqual(["status"]);

  await isolatedDaemon.callAgentTool(SESSION, "open_terminal", { id: "short", title: "Short job", command: "echo finished-output; exit 4" });
  const shortPane = page.locator(".ap-pane", { has: page.locator(".ap-title", { hasText: "Short job" }) });
  await expect(shortPane.locator(".ap-status")).toHaveText("exit 4");
  await expect(shortPane.locator(".ap-output")).toContainText("finished-output");
  await expect(shortPane.locator(".ap-output")).toContainText("[exited with status 4]");

  await statusPane.getByRole("button", { name: "Close" }).click();
  await expect(statusPane).toHaveCount(0);
  await expect.poll(async () => layoutOf(await isolatedDaemon.callAgentTool(SESSION, "get_layout", {})).panes.map((pane) => pane.id)).not.toContain("status");
});

test("the user opens a shell beside the agent from its menu and drags it to the other side", async ({ page, isolatedDaemon }) => {
  await logIn(page);
  await openSession(page);
  const host = page.locator(".term-host");
  await expect(host).toBeVisible();
  const area = (await host.boundingBox())!;
  await page.mouse.click(area.x + area.width / 2, area.y + area.height / 2, { button: "right" });
  const menu = page.getByRole("menu");
  await expect(menu.getByRole("menuitem")).toHaveText(["New shell right", "New shell below", "New shell left", "New shell above"]);
  await menu.getByRole("menuitem", { name: "New shell right" }).click();
  await expect(menu).toHaveCount(0);

  const shell = page.locator(".ap-root:not(.is-warm) .ap-pane.ap-shell");
  await expect(shell).toHaveCount(1);
  await expect.poll(async () => JSON.stringify(layoutOf(await isolatedDaemon.callAgentTool(SESSION, "get_layout", {})).layout))
    .toMatch(/"row":\[\{"pane":"agent"[^\]]*\{"pane":"shell-1"/);

  const head = (await shell.locator(".ap-title").boundingBox())!;
  const agent = (await host.boundingBox())!;
  await page.mouse.move(head.x + 4, head.y + head.height / 2);
  await page.mouse.down();
  await page.mouse.move(head.x + 40, head.y + head.height / 2, { steps: 4 });
  await page.mouse.move(agent.x + agent.width * 0.08, agent.y + agent.height / 2, { steps: 8 });
  await expect(page.locator(".ap-drop-target .pane-drop-region.drop-left")).toBeVisible();
  await page.mouse.up();
  await expect.poll(async () => JSON.stringify(layoutOf(await isolatedDaemon.callAgentTool(SESSION, "get_layout", {})).layout))
    .toMatch(/"row":\[\{"pane":"shell-1"[^\]]*\{"pane":"agent"/);
  await expect.poll(async () => (await shell.boundingBox())!.x).toBeLessThan((await host.boundingBox())!.x);
});

test("terminal panes need bypass permissions while page panes do not", async ({ page, isolatedDaemon }) => {
  await logIn(page);
  isolatedDaemon.setSessionPermission(SESSION, "default");
  await expect(isolatedDaemon.callAgentTool(SESSION, "open_terminal", { id: "job", title: "Job", command: "true" }))
    .rejects.toThrow(/needs bypass permissions/);
  await expect(isolatedDaemon.callAgentTool(SESSION, "send_to_pane", { id: "job", text: "x" }))
    .rejects.toThrow(/needs bypass permissions/);
});

test.describe("page panes without an emulated color scheme", () => {
  // Emulating a scheme overrides prefers-color-scheme in every frame, masking what the iframe passes down.
  test.use({ colorScheme: null });

  test("a page pane follows the user's appearance rather than staying light", async ({ page, isolatedDaemon }) => {
    await logIn(page);
    await openSession(page);
    const cwd = isolatedDaemon.session(SESSION)!.cwd;
    await mkdir(join(cwd, "scheme"), { recursive: true });
    await writeFile(
      join(cwd, "scheme", "index.html"),
      `<!doctype html><h1 id="scheme"></h1><script>
        const query = matchMedia("(prefers-color-scheme: dark)");
        const show = () => { document.getElementById("scheme").textContent = query.matches ? "dark" : "light"; };
        query.addEventListener("change", show);
        show();
      </script>`,
    );
    const html = page.locator("html");
    const toggle = page.locator(".appearance-toggle");
    await toggle.click();
    await expect(html).toHaveAttribute("data-appearance", "light");

    await isolatedDaemon.callAgentTool(SESSION, "open_page", { id: "scheme", title: "Scheme probe", dir: "scheme" });
    const reported = page.frameLocator(".ap-pane iframe").locator("#scheme");
    await expect(reported).toHaveText("light");

    await toggle.click();
    await expect(html).toHaveAttribute("data-appearance", "dark");
    await expect(reported).toHaveText("dark");
  });
});


const SHELL_GEOMETRY_FRAMES = 12;

for (const direction of ["right", "below"] as const) {
  test(`a shell opened ${direction} stays within its pane and accepts input`, async ({ page, isolatedDaemon }) => {
    await logIn(page);
    await openSession(page);
    await page.locator(".term-host").click({ button: "right" });
    await page.getByRole("menuitem", { name: `New shell ${direction}` }).click();
    const shell = page.locator(".ap-root:not(.is-warm) .ap-pane.ap-shell");
    await expect(shell).toHaveCount(1);
    await expect(shell.locator(".ap-body")).not.toHaveClass(/is-curtained/);
    const host = shell.locator(".workspace-terminal-host");
    const samples = await host.evaluate(async (host, count) => {
      const samples: Array<{ host: number; pane: number; canvas: number }> = [];
      for (let frame = 0; frame < count; frame += 1) {
        await new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
        const canvas = host.querySelector("canvas")!;
        samples.push({ host: host.clientWidth, pane: host.closest(".ap-pane")!.getBoundingClientRect().width,
          canvas: canvas.getBoundingClientRect().width });
      }
      return samples;
    }, SHELL_GEOMETRY_FRAMES);
    expect(samples.filter((sample) => sample.host > sample.pane + 1), "a canvas cannot grow its flex host beyond the pane").toEqual([]);
    expect(new Set(samples.map((sample) => sample.host)).size, "shell host width settles instead of growing every frame").toBe(1);
    expect(samples.filter((sample) => sample.canvas > sample.host + 1), "the painted canvas fits its shell host").toEqual([]);
    await expect(shell.locator(".ghostty-input")).toBeFocused();
    await page.keyboard.type("echo shell-geometry-ready\n");
    await expect.poll(async () => JSON.parse(await isolatedDaemon.callAgentTool(SESSION, "read_pane", { id: "shell-1" })).text)
      .toContain("shell-geometry-ready");
    await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title").getByText("browser-e2e-two", { exact: true }) }).click();
    await expect(page).toHaveURL(new RegExp(`#/session/${isolatedDaemon.session("browser-e2e-two")!.id}$`));
  });
}
