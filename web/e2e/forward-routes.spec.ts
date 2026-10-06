import { execFileSync } from "node:child_process";
import { createServer } from "node:http";
import { mkdir, rm, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { expect, test } from "./fixtures";
import { apiHeaders, logIn } from "./support";

test("forward routes load relative assets and isolate opening cookies across previews", async ({ page, browser, isolatedDaemon }) => {
  const received: Array<{ url: string; cookie: string }> = [];
  const target = createServer((request, response) => {
    received.push({ url: request.url ?? "", cookie: request.headers.cookie ?? "" });
    if (request.url?.endsWith("/app.js")) {
      response.writeHead(200, { "content-type": "text/javascript" });
      response.end('fetch("api/status").then(r => r.text()).then(t => document.querySelector("output").textContent = t)');
    } else if (request.url?.endsWith("/api/status")) {
      response.end("preview ready");
    } else {
      response.writeHead(200, { "content-type": "text/html" });
      response.end('<!doctype html><title>Forward preview</title><output>loading</output><script src="app.js"></script>');
    }
  });
  await new Promise<void>((resolve) => target.listen(0, "127.0.0.1", resolve));
  const address = target.address();
  if (!address || typeof address === "string") throw new Error("preview listener unavailable");
  const fresh = await browser.newContext();
  try {
    isolatedDaemon.seedSessionForward("browser-e2e", "first preview", address.port);
    isolatedDaemon.seedSessionForward("browser-e2e-two", "second preview", address.port);
    await isolatedDaemon.restart();
    await logIn(page);
    const urls: string[] = [];
    // The previous session's forward stays mounted for an instant after the
    // click, so each URL is read from the entry naming that session's own
    // forward rather than from whichever link happens to be present.
    for (const [title, label] of [["browser-e2e", "first preview"], ["browser-e2e-two", "second preview"]] as const) {
      await page.locator(".sb-session").filter({ has: page.locator(".sb-session-title").getByText(title, { exact: true }) }).click();
      const link = page.locator(".forward-entry").filter({ hasText: label }).locator(".forward-link");
      await expect(link).toBeVisible();
      const url = await link.getAttribute("href");
      expect(url).toContain("/forwards/");
      urls.push(url!);
    }
    expect(new Set(urls).size).toBe(2);
    const popupEvent = page.waitForEvent("popup");
    await page.locator(".forward-link").click();
    const popup = await popupEvent;
    await expect(popup.locator("output")).toHaveText("preview ready");
    expect(popup.url()).not.toContain("fwd_token");
    await popup.close();

    const signedIn = await browser.newContext({ storageState: await page.context().storageState() });
    const signedOut = await browser.newContext();
    try {
      const destination = `${urls[0]}deep/?x=one%20two&y=3`;
      const direct = await signedIn.newPage();
      await direct.goto(destination);
      await expect(direct.locator("output")).toHaveText("preview ready");
      expect(direct.url()).toBe(destination);
      const login = await signedOut.newPage();
      await login.goto(destination);
      await expect(login.getByRole("button", { name: "log in" })).toBeVisible();
      expect(login.url()).toContain("forward-open");
      await login.locator("input").nth(0).fill("browser-e2e");
      await login.locator("input").nth(1).fill("browser-e2e-password");
      await login.getByRole("button", { name: "log in" }).click();
      await expect(login.locator("output")).toHaveText("preview ready");
      expect(login.url()).toBe(destination);
    } finally {
      await signedIn.close();
      await signedOut.close();
    }

    const previews = [];
    for (const url of urls) {
      const id = new URL(url).pathname.split("/").filter(Boolean).at(-1);
      const minted = await page.request.post(
        `${process.env.PM_E2E_BASE_URL}/api/forwards/${id}/token`,
        { headers: await apiHeaders(page) },
      );
      expect(minted.ok()).toBe(true);
      const { token } = await minted.json();
      const preview = await fresh.newPage();
      await preview.goto(`${url}?fwd_token=${token}`);
      await expect(preview.locator("output")).toHaveText("preview ready");
      expect(preview.url()).toBe(url);
      previews.push(preview);
    }
    for (const preview of previews) {
      await preview.reload();
      await expect(preview.locator("output")).toHaveText("preview ready");
    }
    const cookies = await fresh.cookies();
    expect(cookies.filter(cookie => cookie.name.startsWith("pm_fwd_"))).toHaveLength(2);
    expect(cookies.some(cookie => cookie.name === "pm_session")).toBe(false);
    await isolatedDaemon.restart();
    const rejected = await fresh.request.get(urls[0], {
      headers: { accept: "application/json" },
    });
    expect(rejected.status()).toBe(401);
    expect(await rejected.text()).toContain("expired");
    await previews[0].reload();
    await expect(previews[0].getByRole("button", { name: "log in" })).toBeVisible();
    expect(previews[0].url()).toContain("forward-open");
    const id = new URL(urls[0]).pathname.split("/").filter(Boolean).at(-1);
    const renewed = await page.request.post(
      `${process.env.PM_E2E_BASE_URL}/api/forwards/${id}/token`,
      { headers: await apiHeaders(page) },
    );
    expect(renewed.ok()).toBe(true);
    const { token } = await renewed.json();
    await previews[0].goto(`${urls[0]}?fwd_token=${token}`);
    await expect(previews[0].locator("output")).toHaveText("preview ready");
    expect(received.some(request => request.url?.endsWith("/app.js"))).toBe(true);
    expect(received.some(request => request.url?.endsWith("/api/status"))).toBe(true);
    expect(received.every(request => !request.cookie.includes("pm_"))).toBe(true);
  } finally {
    await fresh.close();
    target.closeAllConnections();
    await new Promise<void>((resolve, reject) => target.close(error => error ? reject(error) : resolve()));
  }
});

test("a supervisor's pane lists its workers' shared URLs with its own and opens them", async ({ page, isolatedDaemon }) => {
  const target = createServer((_request, response) => {
    response.writeHead(200, { "content-type": "text/html" });
    response.end("<!doctype html><title>Worker preview</title><output>worker preview ready</output>");
  });
  await new Promise<void>((resolve) => target.listen(0, "127.0.0.1", resolve));
  const address = target.address();
  if (!address || typeof address === "string") throw new Error("preview listener unavailable");
  try {
    execFileSync(process.env.PM_E2E_PM_BIN!, [
      "spawn", "--project", "1", "--agent", "codex", "--title", "fwd-sub-worker",
    ], { env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET }, encoding: "utf8" });
    isolatedDaemon.setSessionParent("browser-e2e-two", "browser-e2e");
    isolatedDaemon.setSessionParent("fwd-sub-worker", "browser-e2e-two");
    isolatedDaemon.seedSessionForward("browser-e2e", "supervisor preview", address.port);
    isolatedDaemon.seedSessionForward("browser-e2e-two", "worker preview", address.port);
    isolatedDaemon.seedSessionForward("fwd-sub-worker", "sub-worker preview", address.port);
    // A session publishes each port once, so the later entries take ports of their own.
    ["later 1", "later 2", "later 3"].forEach((label, index) => {
      isolatedDaemon.seedSessionForward("browser-e2e", label, address.port + index + 1);
    });
    await isolatedDaemon.restart();
    await logIn(page, { minimumSessions: 3 });

    await page.locator(".sb-session")
      .filter({ has: page.locator(".sb-session-title").getByText("browser-e2e", { exact: true }) })
      .click();
    const bar = page.locator(".forwards-bar");
    // Newest first across the session and its workers, capped at four.
    await expect(bar.locator(".forward-link")).toHaveText([
      "later 3",
      "later 2",
      "later 1",
      "sub-worker preview",
    ]);
    const more = bar.locator(".forwards-more");
    await expect(more).toHaveText("2 more");
    await more.click();
    await expect(bar.locator(".forward-link")).toHaveText([
      "later 3",
      "later 2",
      "later 1",
      "sub-worker preview",
      "worker preview",
      "supervisor preview",
    ]);
    await expect(more).toHaveText("fewer");
    await more.click();
    await expect(bar.locator(".forward-entry")).toHaveCount(4);

    const descendantLink = bar.locator(".forward-entry").filter({ hasText: "sub-worker preview" }).locator(".forward-link");
    const popupEvent = page.waitForEvent("popup");
    await descendantLink.click();
    const popup = await popupEvent;
    await expect(popup.locator("output")).toHaveText("worker preview ready");
    expect(popup.url()).not.toContain("fwd_token");
    await popup.close();

    // A worker's own pane still lists only what it and its own descendants published.
    await page.locator(".sb-session")
      .filter({ has: page.locator(".sb-session-title").getByText("fwd-sub-worker", { exact: true }) })
      .click();
    await expect(page.locator(".forwards-bar .forward-link")).toHaveText(["sub-worker preview"]);
    await expect(page.locator(".forwards-bar .forwards-more")).toHaveCount(0);
  } finally {
    target.closeAllConnections();
    await new Promise<void>((resolve, reject) => target.close(error => error ? reject(error) : resolve()));
  }
});


test("published HTML and Markdown display Unicode without encoding metadata", async ({ page, isolatedDaemon }) => {
  const title = "encoding-preview";
  const root = isolatedDaemon.explicitCwd;
  const files = join(root, "published");
  const text = "Puppet Master is MIT licensed — café 日本語 🎉";
  await mkdir(files);
  await writeFile(join(files, "index.html"), `<!doctype html><p>${text}</p>`);
  await writeFile(join(files, "notes.md"), text);
  await isolatedDaemon.restart({ publicUrl: true });
  execFileSync(process.env.PM_E2E_PM_BIN!, [
    "spawn", "--project", "1", "--agent", "codex", "--title", title, "--cwd", root,
  ], { env: { ...process.env, PM_SOCKET: process.env.PM_E2E_SOCKET }, encoding: "utf8" });
  try {
    const published = await isolatedDaemon.callAgentTool(title, "publish_dir", {
      path: "published", slug: "unicode-preview",
    });
    const match = published.match(/The user-reachable URL is (\S+)/);
    if (!match) throw new Error(`publish_dir did not return a URL: ${published}`);
    await logIn(page, { minimumSessions: 3 });
    const rendered = page.waitForResponse(response => response.url() === match[1] && response.ok());
    await page.goto(match[1]);
    const html = await rendered;
    expect(html.headers()["content-type"]).toBe("text/html; charset=utf-8");
    await expect(page.locator("p")).toHaveText(text);
    expect(await page.evaluate(() => document.characterSet)).toBe("UTF-8");

    const headers = await apiHeaders(page);
    const markdown = await page.request.get(`${match[1]}notes.md`, { headers });
    expect(markdown.ok()).toBe(true);
    expect(markdown.headers()["content-type"]).toBe("text/markdown; charset=utf-8");
    expect(await markdown.text()).toBe(text);
  } finally {
    await isolatedDaemon.callAgentTool(title, "unpublish_dir", { slug: "unicode-preview" });
    await rm(files, { recursive: true, force: true });
  }
});
