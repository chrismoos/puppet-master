import { execFileSync } from "node:child_process";
import { createServer } from "node:http";
import { homedir } from "node:os";
import { dirname, join } from "node:path";
import { expect, test } from "./fixtures";
import { apiHeaders, logIn } from "./support";

const SESSION = "browser-e2e";
const VIEWPORT_WIDTH = 1280;
const VIEWPORT_HEIGHT = 800;
const CONTENT_PIXEL = { x: 20, y: 100 };
const SAVED_COLOR = [20, 130, 80];
const NEXT_PAGE_COLOR = [40, 70, 180];
const RUNTIME_INSTALL_TIMEOUT_MS = 180_000;

test.describe.configure({ mode: "serial" });

test.beforeAll(() => {
  const binary = process.env.PM_E2E_PM_BIN!;
  process.env.PLAYWRIGHT_BROWSERS_PATH ||= join(homedir(), process.platform === "darwin" ? "Library/Caches" : ".cache", "ms-playwright");
  process.env.PM_BROWSER_RUNTIME_DIR = join(dirname(binary), "browser-runtime");
  execFileSync(binary, ["browser", "install"], { env: process.env, stdio: "pipe", timeout: RUNTIME_INSTALL_TIMEOUT_MS });
});

for (const remote of [false, true]) {
test(`managed browser ${remote ? "on a remote worker" : "locally"} appears in a read-only pane and preserves isolated logins`, async ({ page, isolatedDaemon }) => {
  const app = createServer((request, response) => {
    response.setHeader("Content-Type", "text/html");
    response.setHeader("Cross-Origin-Opener-Policy", request.url === "/next" ? "same-origin" : "unsafe-none");
    if (request.url === "/next") {
      response.end('<title>Next document</title><style>body{margin:0;background:rgb(40,70,180)}</style><h1>Next document</h1>');
      return;
    }
    response.end('<title>Managed settings</title><style>body{margin:0;background:rgb(180,40,40)}</style><h1>Settings</h1><input aria-label="Name"><button onclick="document.body.style.background=\'rgb(20,130,80)\';localStorage.setItem(\'account\',document.querySelector(\'input\').value)">Save</button>');
  });
  await new Promise<void>(resolve => app.listen(0, "127.0.0.1", resolve));
  try {
    const address = app.address();
    if (!address || typeof address === "string") throw new Error("test app has no port");
    const url = `http://127.0.0.1:${address.port}`;
    const title = remote ? "remote-browser" : SESSION;
    const call = (name: string, args: Record<string, unknown> = {}) => isolatedDaemon.callAgentTool(title, name, args);
    await logIn(page);
    if (remote) {
      const base = process.env.PM_E2E_BASE_URL!;
      const headers = await apiHeaders(page);
      const enrollment = await page.request.post(`${base}/api/workers/enroll`, { headers, data: { label: "browser-worker" } });
      expect(enrollment.ok()).toBeTruthy();
      const { token } = await enrollment.json() as { token: string };
      await isolatedDaemon.startRemoteWorker(token);
      await expect.poll(() => isolatedDaemon.workerId("browser-worker")).not.toBeNull();
      const worker = isolatedDaemon.workerId("browser-worker")!;
      const assignment = await page.request.post(`${base}/api/buckets/1/worker`, { headers, data: { worker_id: 0, allowed_worker_ids: [0, worker] } });
      expect(assignment.ok()).toBeTruthy();
      const project = await page.request.post(`${base}/api/projects/1/worker`, { headers, data: { worker_id: 0, allowed_worker_ids: [0, worker] } });
      expect(project.ok()).toBeTruthy();
      execFileSync(process.env.PM_E2E_PM_BIN!, ["--socket", process.env.PM_E2E_SOCKET!, "spawn", "--project", "1", "--agent", "codex", "--worker", String(worker), "--title", title], { env: process.env, encoding: "utf8" });
      await expect.poll(() => isolatedDaemon.session(title)?.workerId).toBe(worker);
    }
    await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title").getByText(title, { exact: true }) }).click();
    await expect(page).toHaveURL(/#\/session\/\d+$/);
    await call("browser_navigate", { url });
    await call("show_browser", { title: "Account browser" });
    const pane = page.locator(".ap-browser");
    await expect(pane).toBeVisible();
    await expect(pane.getByRole("status")).toHaveText("Live · read-only");
    await expect(pane.locator(".browser-caption")).toContainText(`${VIEWPORT_WIDTH} × ${VIEWPORT_HEIGHT}`);
    expect(await pane.locator("iframe").count()).toBe(0);
    expect(await pane.getByRole("button", { name: "Allow typing" }).count()).toBe(0);
    await call("browser_type", { target: "input", text: "First account" });
    await call("browser_click", { target: "button" });
    await expect.poll(() => pane.locator("canvas").evaluate((canvas, point) => {
      const context = canvas.getContext("2d");
      return context ? Array.from(context.getImageData(point.x, point.y, 1, 1).data).slice(0, 3) : [];
    }, CONTENT_PIXEL)).toEqual(SAVED_COLOR.map(value => expect.closeTo(value, -1)));
    const originalCanvas = await pane.locator("canvas").elementHandle();
    for (const browserTitle of ["Account browser", "Signed-in account"]) {
      await call("show_browser", { title: browserTitle });
      await expect(pane.locator(".ap-title")).toHaveText(browserTitle);
      expect(await pane.locator("canvas").evaluate((canvas, original) => canvas === original, originalCanvas)).toBe(true);
      await expect(pane.getByRole("status")).toHaveText("Live · read-only");
    }
    await call("browser_navigate", { url: url.replace("127.0.0.1", "localhost") + "/next" });
    await expect.poll(() => pane.locator("canvas").evaluate((canvas, point) => {
      return Array.from(canvas.getContext("2d")!.getImageData(point.x, point.y, 1, 1).data).slice(0, 3);
    }, CONTENT_PIXEL)).toEqual(NEXT_PAGE_COLOR.map(value => expect.closeTo(value, -1)));
    await call("browser_navigate_back");
    const actualColorText = await call("browser_evaluate", { function: '() => getComputedStyle(document.body).backgroundColor.match(/\\d+/g).slice(0, 3).map(Number)' });
    const actualColor = JSON.parse(actualColorText.split("### Result\n")[1].split("\n###")[0]) as number[];
    await expect.poll(() => pane.locator("canvas").evaluate((canvas, point) => {
      return Array.from(canvas.getContext("2d")!.getImageData(point.x, point.y, 1, 1).data).slice(0, 3);
    }, CONTENT_PIXEL)).toEqual(actualColor.map(value => expect.closeTo(value, -1)));
    await pane.getByRole("button", { name: "Hide", exact: true }).click();
    await expect(pane).toHaveCount(0);
    await expect(call("browser_evaluate", { function: '() => localStorage.getItem("account")' })).resolves.toContain("First account");
    await call("browser_evaluate", { function: '() => { document.body.style.background = "rgb(40,70,180)"; }' });
    await call("show_browser", { title: "Account browser" });
    await expect(pane.getByRole("status")).toHaveText("Live · read-only");
    await expect.poll(() => pane.locator("canvas").evaluate((canvas, point) => {
      return Array.from(canvas.getContext("2d")!.getImageData(point.x, point.y, 1, 1).data).slice(0, 3);
    }, CONTENT_PIXEL)).toEqual(NEXT_PAGE_COLOR.map(value => expect.closeTo(value, -1)));
    await expect(page.locator(".ap-browser")).toHaveCount(1);
    await call("browser_contexts", { action: "create", browser_context: "other-user", reason: "Verify another signed-in account" });
    await call("browser_navigate", { url, browser_context: "other-user" });
    await expect(call("browser_evaluate", { browser_context: "other-user", function: '() => localStorage.getItem("account")' })).resolves.toContain("null");
    await call("browser_tabs", { action: "new", url: url + "/another-tab" });
    await expect(pane.locator(".browser-url")).toContainText("/another-tab");
    await call("browser_resize", { width: 900, height: 600 });
    await expect(pane.locator(".browser-caption")).toContainText("900 × 600");
    const previousBrowserCanvas = await pane.locator("canvas").elementHandle();
    await call("browser_contexts", { action: "close" });
    await call("browser_navigate", { url });
    await call("show_browser", { title: "Replacement browser" });
    await expect(pane.locator(".ap-title")).toHaveText("Replacement browser");
    expect(await pane.locator("canvas").evaluate((canvas, previous) => canvas === previous, previousBrowserCanvas)).toBe(false);
    await expect(pane.getByRole("status")).toHaveText("Live · read-only");
    await expect(pane.locator(".browser-caption")).toContainText(`${VIEWPORT_WIDTH} × ${VIEWPORT_HEIGHT}`);
    await call("close_pane", { id: "browser-default" });
    await expect(pane).toHaveCount(0);
    await call("browser_contexts", { action: "close", browser_context: "other-user" });
    await call("browser_contexts", { action: "close" });
    await expect(call("browser_contexts", { action: "list" })).resolves.toContain('"contexts":[]');
  } finally {
    await new Promise<void>(resolve => app.close(() => resolve()));
  }
});
}
