import { expect, test, type BrowserContext, type Page } from "./fixtures";
import { apiHeaders, logIn } from "./support";
import { fileURLToPath } from "node:url";

const USERNAME = "browser-e2e";
const EXTENSIONLESS_GHOSTTY_FIXTURE = fileURLToPath(new URL("./fixtures/Dracula", import.meta.url));

type ThemeProbeWindow = Window & {
  __themeProbe: { created: number; closed: number; resize: number; replay: number; urls: string[] };
  __themeXterm?: Element;
  __themeTextarea?: Element;
  __themeWorkspaceXterm?: Element;
  __themeWorkspaceTextarea?: Element;
};

async function installTerminalProbe(context: BrowserContext): Promise<void> {
  await context.addInitScript(() => {
    const probe = { created: 0, closed: 0, resize: 0, replay: 0, urls: [] as string[] };
    (window as ThemeProbeWindow).__themeProbe = probe;
    const NativeWebSocket = window.WebSocket;
    window.WebSocket = new Proxy(NativeWebSocket, {
      construct(target, args) {
        const socket = Reflect.construct(target, args) as WebSocket;
        const terminal = String(args[0]).includes("/ws/terminal/");
        if (terminal) {
          probe.created += 1;
          probe.urls.push(String(args[0]));
          socket.addEventListener("close", () => probe.closed += 1);
          socket.addEventListener("message", (event) => {
            if (!(event.data instanceof ArrayBuffer) || event.data.byteLength < 10) return;
            const view = new DataView(event.data);
            if (view.getUint8(0) === 1 && (view.getUint8(9) & 4) !== 0) probe.replay += 1;
          });
          const nativeSend = socket.send.bind(socket);
          socket.send = (data: string | ArrayBufferLike | Blob | ArrayBufferView) => {
            const bytes = data instanceof ArrayBuffer
              ? new Uint8Array(data)
              : ArrayBuffer.isView(data)
                ? new Uint8Array(data.buffer, data.byteOffset, data.byteLength)
                : null;
            if (bytes?.[0] === 3 || bytes?.[0] === 8) probe.resize += 1;
            nativeSend(data);
          };
        }
        return socket;
      },
    }) as typeof WebSocket;
  });
}

/// What the bar under the preview says: which theme is applied, and which
/// one is being previewed over it. The picker lists every bundled palette
/// by name, so a bare text match on one of them does not identify either.
function previewBar(page: Page) {
  return page.locator(".set-preview-bar .msg");
}

/// Long enough that a pending viewer-size assert, debounced by
/// VIEWER_SIZE_ASSERT_DEBOUNCE_MS, has landed before the counters are
/// compared again.
const PROBE_SETTLE_MS = 500;

function terminalProbe(page: Page): Promise<Record<string, unknown>> {
  return page.evaluate(() => ({ ...(window as ThemeProbeWindow).__themeProbe }));
}

async function openTerminalTheme(page: Page): Promise<void> {
  await page.getByRole("button", { name: `Account menu for ${USERNAME}` }).click();
  await expect(page.getByRole("menu")).toBeVisible();
  await expect(page.getByRole("menuitem", { name: "Log out" })).toBeVisible();
  // The account menu signs out and nothing else: the gear opens Settings.
  await expect(page.getByRole("menuitem")).toHaveCount(1);
  await page.keyboard.press("Escape");
  await page.getByRole("link", { name: "Settings", exact: true }).click();
  await page.getByRole("navigation", { name: "Settings" }).getByRole("link", { name: "Terminal theme" }).click();
  await expect(page.getByRole("heading", { level: 2, name: "Terminal theme" })).toBeVisible();
}

test("account terminal themes preview locally, sync live, preserve xterm, export, and reset", async ({ page, context }) => {
  await installTerminalProbe(context);
  await logIn(page);
  await page.request.delete(`${process.env.PM_E2E_BASE_URL!}/api/user/settings/terminal-theme`, {
    headers: await apiHeaders(page),
  });

  // Keep one warm TerminalStage layer live while another same-user client
  // imports and applies a theme.
  await page.locator(".sb-session").first().click();
  await expect(page.locator(".xterm")).toBeVisible();
  await page.evaluate(() => {
    const current = window as ThemeProbeWindow;
    current.__themeXterm = document.querySelector(".term-layer[style*='visibility: visible'] .xterm")!;
    current.__themeTextarea = document.querySelector(".term-layer[style*='visibility: visible'] .xterm-helper-textarea")!;
  });
  await page.locator('button[title="new workspace"]').click();
  await page.locator(".workspace-name-modal input").fill("theme lifecycle");
  await page.getByRole("button", { name: "create" }).click();
  await expect(page.locator(".workspace-terminal-host .xterm")).toBeVisible();
  await expect.poll(() => page.evaluate(() => {
    const probe = (window as ThemeProbeWindow).__themeProbe;
    return { created: probe.created, replay: probe.replay };
  })).toEqual({ created: 2, replay: 2 });
  await expect.poll(async () => {
    const before = await terminalProbe(page);
    // e2e-real-time-wait: quiescence is the condition, and nothing signals it.
    await page.waitForTimeout(PROBE_SETTLE_MS);
    return JSON.stringify(before) === JSON.stringify(await terminalProbe(page));
  }).toBe(true);
  const baseline = await page.evaluate(() => {
    const current = window as ThemeProbeWindow;
    current.__themeWorkspaceXterm = document.querySelector(".workspace-terminal-host .xterm")!;
    current.__themeWorkspaceTextarea = document.querySelector(".workspace-terminal-host .xterm-helper-textarea")!;
    return {
      ...current.__themeProbe,
    };
  });

  const editor = await context.newPage();
  await editor.goto(process.env.PM_E2E_BASE_URL!);
  await expect(editor.locator(".sb-session")).toHaveCount(2);
  await openTerminalTheme(editor);
  await expect(previewBar(editor)).toHaveText("Puppet Master is active.");

  const observer = await context.newPage();
  await observer.goto(process.env.PM_E2E_BASE_URL!);
  await expect(observer.locator(".sb-session")).toHaveCount(2);
  await openTerminalTheme(observer);

  const picker = editor.locator("input[type=file]");
  await expect(picker).not.toHaveAttribute("accept");
  await picker.setInputFiles(EXTENSIONLESS_GHOSTTY_FIXTURE);
  await expect(previewBar(editor)).toHaveText("Previewing Dracula. Your terminals still use Puppet Master.");
  await expect(editor.getByLabel("Dracula terminal color preview").locator(".screen"))
    .toHaveCSS("background-color", "rgb(40, 42, 54)");
  // Preview remains local: the other connected Settings page still shows the
  // persisted built-in theme until Apply.
  await expect(previewBar(observer)).toHaveText("Puppet Master is active.");

  await editor.getByRole("button", { name: "Cancel", exact: true }).click();
  await expect(previewBar(editor)).toHaveText("Puppet Master is active.");
  await picker.setInputFiles({
    name: "diagnostics.conf",
    mimeType: "text/plain",
    buffer: Buffer.from([
      "background = #101820",
      "background = #12202a",
      "include = ~/.config/ghostty/not-evaluated",
    ].join("\n")),
  });
  await expect(editor.getByRole("status", { name: "theme import warnings" })).toContainText("duplicate background");
  await expect(editor.getByRole("status", { name: "theme import warnings" })).toContainText("ignored Ghostty key \"include\"");
  await editor.getByRole("button", { name: "Cancel", exact: true }).click();
  await picker.setInputFiles(EXTENSIONLESS_GHOSTTY_FIXTURE);
  await editor.getByRole("button", { name: "Apply theme" }).click();
  await expect(editor.getByText(/Dracula applied and synced/i)).toBeVisible();
  await expect(previewBar(observer)).toHaveText("Dracula is active.");

  await expect.poll(async () => page.evaluate(() => {
    const current = window as ThemeProbeWindow;
    return {
      warmStageRetained: current.__themeXterm?.contains(current.__themeTextarea ?? null) === true
        && current.__themeXterm.closest(".term-layer")?.parentElement?.classList.contains("term-stage") === true,
      sameWorkspaceXterm: current.__themeWorkspaceXterm === document.querySelector(".workspace-terminal-host .xterm"),
      sameWorkspaceTextarea: current.__themeWorkspaceTextarea === document.querySelector(".workspace-terminal-host .xterm-helper-textarea"),
      created: current.__themeProbe.created,
      closed: current.__themeProbe.closed,
      resize: current.__themeProbe.resize,
      replay: current.__themeProbe.replay,
      urls: current.__themeProbe.urls,
    };
  })).toEqual({
    warmStageRetained: true,
    sameWorkspaceXterm: true,
    sameWorkspaceTextarea: true,
    created: baseline.created,
    closed: baseline.closed,
    resize: baseline.resize,
    replay: baseline.replay,
    urls: baseline.urls,
  });

  const download = editor.waitForEvent("download");
  await editor.getByRole("button", { name: "Export JSON" }).click();
  expect((await download).suggestedFilename()).toBe("dracula.json");

  await editor.getByRole("button", { name: "Reset" }).click();
  await expect(editor.getByText(/Built-in Puppet Master terminal theme restored/i)).toBeVisible();
  await expect(previewBar(observer)).toHaveText("Puppet Master is active.");
  for (const client of [editor, observer]) {
    await client.getByRole("button", { name: "Back to sessions" }).click();
    await expect(client.getByRole("navigation", { name: "Settings" })).toHaveCount(0);
  }
});
