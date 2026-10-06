import { expect, test, type Page } from "./fixtures";
import { logIn } from "./support";

function base64(text: string): string {
  return Buffer.from(text, "utf8").toString("base64");
}

async function openTerminal(page: Page) {
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await expect(page).toHaveURL(/#\/session\/\d+$/);
  const input = page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
  await input.focus();
  return input;
}

const SESSION_HOST = '.term-layer[style*="visible"]';
const WORKSPACE_HOST = ".workspace-terminal-host";

async function sendOsc52(page: Page, data: string, host = SESSION_HOST): Promise<void> {
  const input = page.locator(`${host} .xterm-helper-textarea`);
  await input.pressSequentially(`osc52 ${data}`);
  await input.press("Enter");
}

/** Opens a split-pane workspace showing the seeded browser-e2e terminal. */
async function openWorkspacePane(page: Page) {
  await logIn(page);
  await page.getByTitle("new workspace").click();
  await page.getByLabel("name", { exact: true }).fill("clipboard workspace");
  await page.getByRole("button", { name: "create workspace" }).click();
  await page.getByLabel("terminal shown in pane").selectOption({ index: 2 });
  await expect(page.locator(`${WORKSPACE_HOST} .xterm`)).toBeVisible();
  const input = page.locator(`${WORKSPACE_HOST} .xterm-helper-textarea`);
  await input.focus();
  return input;
}

async function paneCellGeometry(page: Page, host: string) {
  return page.locator(`${host} .xterm`).evaluate((element) => {
    const screen = element.querySelector<HTMLElement>(".xterm-screen")!;
    const input = element.querySelector<HTMLTextAreaElement>(".xterm-helper-textarea")!;
    const rect = screen.getBoundingClientRect();
    const inputRect = input.getBoundingClientRect();
    return {
      left: rect.left,
      cursorTop: inputRect.top,
      cellWidth: inputRect.width,
      cellHeight: inputRect.height,
    };
  });
}

/**
 * Replays Safari's clipboard policy inside Chromium: a write succeeds only
 * when it is issued synchronously from a user gesture. Chromium itself
 * grants gesture-free writes to a focused page once the permission is
 * granted, which the immediate-success test relies on.
 */
async function requireSynchronousGesture(page: Page): Promise<void> {
  await page.addInitScript(() => {
    let inGesture = false;
    for (const type of ["pointerdown", "keydown"]) {
      document.addEventListener(type, () => {
        inGesture = true;
        setTimeout(() => { inGesture = false; }, 0);
      }, { capture: true });
    }
    const clipboard = navigator.clipboard;
    const original = clipboard.writeText.bind(clipboard);
    const calls: Array<{ text: string; inGesture: boolean }> = [];
    const probe = window as typeof window & {
      __clipboardWrites?: typeof calls;
      __clipboardSeed?: (text: string) => Promise<void>;
    };
    probe.__clipboardWrites = calls;
    probe.__clipboardSeed = original;
    clipboard.writeText = (text: string) => {
      calls.push({ text, inGesture });
      if (!inGesture) {
        const error = new Error("Document is not focused.");
        error.name = "NotAllowedError";
        return Promise.reject(error);
      }
      return original(text);
    };
  });
}

async function seedClipboard(page: Page, text: string): Promise<void> {
  await page.evaluate((seed) => (
    (window as typeof window & { __clipboardSeed?: (text: string) => Promise<void> }).__clipboardSeed!(seed)
  ), text);
}

async function clipboardWrites(page: Page): Promise<Array<{ text: string; inGesture: boolean }>> {
  return page.evaluate(() => (
    (window as typeof window & { __clipboardWrites?: Array<{ text: string; inGesture: boolean }> }).__clipboardWrites ?? []
  ));
}

test("an OSC 52 copy the browser accepts reaches the clipboard immediately", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await openTerminal(page);
  const status = page.locator(".terminal-clipboard-status");
  const text = "copied straight from the agent";

  await sendOsc52(page, `c;${base64(text)}`);
  await expect(status).toBeVisible();
  await expect(status).toHaveClass(/is-copied/);
  await expect(status).toContainText(`Copied ${text.length} chars`);
  await expect(status.getByRole("button")).toBeHidden();
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(text);
  await expect(status).toBeHidden({ timeout: 10_000 });
});

test("an OSC 52 copy refused without activation is parked and completes on the next gesture", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await requireSynchronousGesture(page);
  await openTerminal(page);
  await seedClipboard(page, "stale");
  const status = page.locator(".terminal-clipboard-status");
  const text = "parked until a gesture — 日本";

  await sendOsc52(page, `c;${base64(text)}`);
  await expect(status).toBeVisible();
  await expect(status).toHaveClass(/is-pending/);
  await expect(status).toContainText(`Copy of ${text.length} chars is waiting for a click or keypress`);
  await expect(status.getByRole("button", { name: "copy now" })).toBeVisible();
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe("stale");
  expect((await clipboardWrites(page)).at(-1)).toEqual({ text, inGesture: false });

  await status.getByRole("button", { name: "copy now" }).click();
  await expect(status).toHaveClass(/is-copied/);
  await expect(status).toContainText(`Copied ${text.length} chars`);
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(text);
  expect((await clipboardWrites(page)).at(-1)).toEqual({ text, inGesture: true });

  const second = "a plain click on the terminal also completes";
  await sendOsc52(page, `c;${base64(second)}`);
  await expect(status).toHaveClass(/is-pending/);
  const terminal = page.locator('.term-layer[style*="visible"] .xterm-screen');
  const box = await terminal.boundingBox();
  await page.mouse.click(box!.x + box!.width / 2, box!.y + box!.height / 2);
  await expect(status).toHaveClass(/is-copied/);
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(second);
});

test("an OSC 52 read request is ignored and never touches the clipboard", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await requireSynchronousGesture(page);
  await openTerminal(page);
  await seedClipboard(page, "private");
  const status = page.locator(".terminal-clipboard-status");

  await sendOsc52(page, "c;?");
  await sendOsc52(page, `c;${base64("marker after the read")}`);
  await expect(status).toHaveClass(/is-pending/);
  expect((await clipboardWrites(page)).map((call) => call.text)).toEqual(["marker after the read"]);
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe("private");
});

test("a workspace pane routes an agent OSC 52 copy to the clipboard", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await openWorkspacePane(page);
  const status = page.locator(`${WORKSPACE_HOST} .terminal-clipboard-status`);
  const text = "copied from a workspace pane";

  await sendOsc52(page, `c;${base64(text)}`, WORKSPACE_HOST);
  await expect(status).toBeVisible();
  await expect(status).toHaveClass(/is-copied/);
  await expect(status).toContainText(`Copied ${text.length} chars`);
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(text);
});

test("a workspace pane parks a refused OSC 52 copy and completes it on a click", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  await requireSynchronousGesture(page);
  await openWorkspacePane(page);
  await seedClipboard(page, "stale");
  const status = page.locator(`${WORKSPACE_HOST} .terminal-clipboard-status`);
  const text = "parked in a workspace pane";

  await sendOsc52(page, `c;${base64(text)}`, WORKSPACE_HOST);
  await expect(status).toHaveClass(/is-pending/);
  expect(await page.evaluate(() => navigator.clipboard.readText())).toBe("stale");

  const screen = page.locator(`${WORKSPACE_HOST} .xterm-screen`);
  const box = await screen.boundingBox();
  await page.mouse.click(box!.x + box!.width / 2, box!.y + box!.height / 2);
  await expect(status).toHaveClass(/is-copied/);
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(text);
});

test("a workspace pane copies on select and on the copy shortcut", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read", "clipboard-write"]);
  const input = await openWorkspacePane(page);
  const marker = "WORKSPACE-SELECTION-MARKER";
  await input.pressSequentially(`echo ${marker}`);
  await input.press("Enter");

  // The WebGL renderer paints no DOM rows, so drag across where the echo
  // lands (the row above the cursor, after the "OUT " prefix) until the
  // selection copy reports the marker.
  await expect.poll(async () => {
    const geometry = await paneCellGeometry(page, WORKSPACE_HOST);
    const rowY = geometry.cursorTop - geometry.cellHeight / 2;
    const startX = geometry.left + geometry.cellWidth * 4;
    const endX = startX + geometry.cellWidth * marker.length;
    await page.mouse.move(startX, rowY);
    await page.mouse.down();
    await page.mouse.move(endX, rowY, { steps: 6 });
    await page.mouse.up();
    return page.evaluate(() => navigator.clipboard.readText());
  }, { message: "select-to-copy in a workspace pane", timeout: 10_000 }).toBe(marker);

  await page.evaluate(() => navigator.clipboard.writeText("replaced"));
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe("replaced");
  await input.press("Control+Shift+C");
  await expect.poll(() => page.evaluate(() => navigator.clipboard.readText())).toBe(marker);
  await expect(page.locator(`${WORKSPACE_HOST} .terminal-clipboard-status`)).toBeHidden();
});

/**
 * A plain-HTTP origin has no navigator.clipboard at all, and its legacy
 * copy command succeeds only inside a user gesture. Automated Chromium
 * accepts the command gesture-free, so the gesture rule is replayed here
 * while an in-gesture copy still runs the real command.
 */
async function removeAsyncClipboard(page: Page): Promise<void> {
  await page.addInitScript(() => {
    const clipboard = navigator.clipboard;
    (window as typeof window & { __readClipboard?: () => Promise<string> }).__readClipboard =
      () => clipboard.readText();
    Object.defineProperty(navigator, "clipboard", { configurable: true, value: undefined });
    let inGesture = false;
    for (const type of ["pointerdown", "keydown"]) {
      document.addEventListener(type, () => {
        inGesture = true;
        setTimeout(() => { inGesture = false; }, 0);
      }, { capture: true });
    }
    const original = document.execCommand.bind(document);
    document.execCommand = (command: string, ...rest: unknown[]) => (
      command === "copy" && !inGesture ? false : (original as (...args: unknown[]) => boolean)(command, ...rest)
    );
  });
}

test("on an origin without navigator.clipboard an OSC 52 copy is parked, explained, and completed by a click", async ({ page, context }) => {
  await context.grantPermissions(["clipboard-read"]);
  await removeAsyncClipboard(page);
  await openTerminal(page);
  expect(await page.evaluate(() => navigator.clipboard)).toBeUndefined();
  const readClipboard = () => page.evaluate(() => (
    (window as typeof window & { __readClipboard?: () => Promise<string> }).__readClipboard!()
  ));
  const status = page.locator(".terminal-clipboard-status");
  const text = "copied over plain http";

  await sendOsc52(page, `c;${base64(text)}`);
  await expect(status).toBeVisible();
  await expect(status).toHaveClass(/is-pending/);
  await expect(status).toContainText("not HTTPS or localhost");
  await expect(status.getByRole("button", { name: "copy now" })).toBeVisible();

  await status.getByRole("button", { name: "copy now" }).click();
  await expect(status).toHaveClass(/is-copied/);
  await expect(status).toContainText(`Copied ${text.length} chars`);
  await expect.poll(readClipboard).toBe(text);
});
