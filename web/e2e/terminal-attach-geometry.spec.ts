import { expect, test, type Browser, type Page } from "./fixtures";
import { apiHeaders, expectTerminalRevealed, logIn } from "./support";

// gridtui (pm-testagent) lays out numbered rows exactly as wide as the PTY, a
// rule, a prompt, and one status line, repaints on SIGWINCH, and otherwise
// redraws its footer relative to the cursor. A viewer that paints bytes
// shaped for a different geometry shows a torn or doubled footer.
const GRID_COMMAND = "gridtui";
const FOOTER_ROWS = 3;
const PROMPT_COLUMN = 2;
const PHONE_COLS = 50;
const PHONE_ROWS = 20;
const VIEWER_HEIGHT = 900;
const WIDE_VIEWER = 1600;
const NARROW_VIEWER = 1100;
const EQUAL_VIEWER = 1400;
const SETTLE_TIMEOUT_MS = 8_000;
/** Footer redraws to observe after the screen is right, so a frame that is
 * only momentarily correct cannot pass. */
const STABLE_TICKS = 6;
const TERMINAL_SUBPROTOCOL = "pm-terminal-v1";
const TERMINAL_TAG_OUTPUT = 0x01;
const TERMINAL_PAYLOAD_OFFSET = 10;

interface StageWindow extends Window {
  __pmStage?: {
    debugSnapshot(): Array<{
      key: string;
      visible: boolean;
      socket: { ptySize: { cols: number; rows: number } | null } | null;
    }>;
    layers: Map<string, {
      term: {
        cols: number;
        rows: number;
        buffer: {
          active: {
            baseY: number;
            cursorX: number;
            cursorY: number;
            getLine(row: number): {
              isWrapped: boolean;
              translateToString(trimRight?: boolean): string;
            } | undefined;
          };
        };
      };
    }>;
  };
}

interface Screen {
  cols: number;
  rows: number;
  lines: string[];
  wrapped: boolean[];
  cursorX: number;
  cursorY: number;
  ptySize: { cols: number; rows: number } | null;
}

async function readScreen(page: Page): Promise<Screen | null> {
  return page.evaluate(() => {
    const stage = (window as StageWindow).__pmStage;
    const visible = stage?.debugSnapshot().find((layer) => layer.visible);
    const layer = visible ? stage?.layers.get(visible.key) : undefined;
    if (!visible || !layer) return null;
    const buffer = layer.term.buffer.active;
    const lines: string[] = [];
    const wrapped: boolean[] = [];
    for (let row = buffer.baseY; row < buffer.baseY + layer.term.rows; row += 1) {
      const line = buffer.getLine(row);
      lines.push(line?.translateToString(true) ?? "");
      wrapped.push(line?.isWrapped ?? false);
    }
    return {
      cols: layer.term.cols,
      rows: layer.term.rows,
      lines,
      wrapped,
      cursorX: buffer.cursorX,
      cursorY: buffer.cursorY,
      ptySize: visible.socket?.ptySize ?? null,
    };
  });
}

/** Every way the visible screen differs from what gridtui paints for the
 * xterm's own size. Empty means the frame is exactly right. */
function gridProblems(screen: Screen | null): string[] {
  if (!screen) return ["no visible terminal layer"];
  const { cols, rows, lines } = screen;
  const size = `${cols}x${rows}`;
  const problems: string[] = [];
  if (!screen.ptySize || screen.ptySize.cols !== cols || screen.ptySize.rows !== rows) {
    problems.push(`daemon reports PTY ${screen.ptySize ? `${screen.ptySize.cols}x${screen.ptySize.rows}` : "unknown"}, xterm is ${size}`);
  }
  const statusRows = lines.flatMap((line, row) => (line.includes("STATUS ") ? [row] : []));
  if (statusRows.length !== 1) {
    problems.push(`${statusRows.length} status lines on screen (rows ${statusRows.join(", ")}), expected exactly 1`);
  }
  const gridRows = rows - FOOTER_ROWS;
  for (let row = 0; row < gridRows; row += 1) {
    const label = `GRID-${String(row + 1).padStart(3, "0")} ${size} `;
    const line = lines[row] ?? "";
    if (!line.startsWith(label) || line.length !== cols || !line.endsWith("#")) {
      problems.push(`row ${row} is not "${label}…#" at ${cols} columns (length ${line.length})`);
    }
  }
  const rule = lines[gridRows] ?? "";
  if (rule !== "─".repeat(cols)) problems.push(`row ${gridRows} is not a ${cols}-column rule`);
  if ((lines[gridRows + 1] ?? "").trim() !== ">") problems.push(`row ${gridRows + 1} is not the prompt`);
  const status = lines[gridRows + 2] ?? "";
  if (!status.startsWith(`STATUS ${size} `) || status.length !== cols) {
    problems.push(`row ${gridRows + 2} is not the ${size} status line at ${cols} columns`);
  }
  const wrappedRows = screen.wrapped.flatMap((wrapped, row) => (wrapped ? [row] : []));
  if (wrappedRows.length > 0) problems.push(`rows ${wrappedRows.join(", ")} are soft-wrapped`);
  if (screen.cursorY !== gridRows + 1 || screen.cursorX !== PROMPT_COLUMN) {
    problems.push(`cursor at row ${screen.cursorY} column ${screen.cursorX}, expected row ${gridRows + 1} column ${PROMPT_COLUMN}`);
  }
  return problems;
}

function describe(screen: Screen | null): string {
  if (!screen) return "(no screen)";
  const pty = screen.ptySize ? `${screen.ptySize.cols}x${screen.ptySize.rows}` : "unknown";
  const header = `xterm ${screen.cols}x${screen.rows}, daemon PTY ${pty}, cursor ${screen.cursorY}:${screen.cursorX}`;
  const body = screen.lines.map((line, row) => `${String(row).padStart(3)}${screen.wrapped[row] ? "~" : "|"}${line}`);
  return [header, ...body].join("\n");
}

function statusTick(screen: Screen | null): number {
  const status = screen?.lines.find((line) => line.includes("STATUS "));
  const match = status ? / tick (\d+)/.exec(status) : null;
  return match ? Number(match[1]) : -1;
}

/**
 * Waits for gridtui's frame to be right in this viewer, then keeps checking
 * across several footer redraws. The assertion carries the whole screen, so
 * a failure shows the doubled status line or the wrong width directly.
 */
async function expectGridFrame(page: Page, when: string): Promise<void> {
  let screen: Screen | null = null;
  await expect.poll(async () => {
    screen = await readScreen(page);
    return gridProblems(screen);
  }, { timeout: SETTLE_TIMEOUT_MS }).toEqual([]).catch(() => {});
  expect(gridProblems(screen), `${when}:\n${describe(screen)}`).toEqual([]);

  const settledTick = statusTick(screen);
  await expect.poll(async () => {
    screen = await readScreen(page);
    return statusTick(screen);
  }, { timeout: SETTLE_TIMEOUT_MS }).toBeGreaterThanOrEqual(settledTick + STABLE_TICKS);
  expect(gridProblems(screen), `${when}, after the footer redrew:\n${describe(screen)}`).toEqual([]);
}

async function openViewer(browser: Browser, width: number): Promise<Page> {
  const context = await browser.newContext({ viewport: { width, height: VIEWER_HEIGHT } });
  const page = await context.newPage();
  await logIn(page);
  return page;
}

async function openSession(page: Page): Promise<void> {
  await page.locator(".sb-session").nth(0).click();
  await expect(page.locator('.term-layer[style*="visible"] .xterm')).toBeVisible();
  await expectTerminalRevealed(page, "s:");
}

/** Opens the session in a viewer, starts gridtui, and returns the terminal
 * socket path the viewer attached with. */
async function startGrid(page: Page): Promise<string> {
  const socketUrl = page.waitForEvent("websocket", (socket) => socket.url().includes("/ws/terminal/"));
  await openSession(page);
  const url = new URL((await socketUrl).url());
  const textarea = page.locator('.term-layer[style*="visible"] .xterm-helper-textarea');
  await textarea.focus();
  await page.keyboard.type(GRID_COMMAND);
  await page.keyboard.press("Enter");
  await expectGridFrame(page, "the viewer that started gridtui");
  return `${url.pathname}?generation=${url.searchParams.get("generation")}`;
}

async function closeViewer(page: Page): Promise<void> {
  await page.context().close();
}

/**
 * Attaches the way the phone client does: a bare terminal socket carrying
 * the phone's size. Resolves once gridtui has repainted for that size, and
 * returns the PTY size the daemon echoed.
 */
async function attachAsPhone(page: Page, terminalPath: string): Promise<{ cols: number; rows: number }> {
  const base = process.env.PM_E2E_BASE_URL as string;
  const terminal = new URL(terminalPath, base);
  const terminalId = terminal.pathname.split("/").at(-1)!;
  const response = await page.request.post(`${base}/api/terminals/${terminalId}/attach-ticket`, {
    headers: await apiHeaders(page),
    data: { generation: terminal.searchParams.get("generation") },
  });
  expect(response.ok()).toBe(true);
  const { ticket } = await response.json() as { ticket: string };
  terminal.searchParams.set("ticket", ticket);
  terminalPath = terminal.pathname + terminal.search;
  return page.evaluate(async ({ path, cols, rows, protocol, tag, offset }) => {
    const url = new URL(`${path}&cols=${cols}&rows=${rows}`, location.href);
    url.protocol = url.protocol === "https:" ? "wss:" : "ws:";
    const socket = new WebSocket(url, protocol);
    socket.binaryType = "arraybuffer";
    const decoder = new TextDecoder();
    let text = "";
    let echoed = { cols: 0, rows: 0 };
    const marker = `STATUS ${cols}x${rows} `;
    await new Promise<void>((resolve, reject) => {
      socket.onerror = () => reject(new Error("phone socket failed"));
      socket.onmessage = (event) => {
        const view = new DataView(event.data as ArrayBuffer);
        if (view.getUint8(0) === tag) {
          text += decoder.decode(new Uint8Array(event.data as ArrayBuffer, offset), { stream: true });
        } else if (view.byteLength === 13) {
          echoed = { cols: view.getUint16(9, true), rows: view.getUint16(11, true) };
        }
        if (text.includes(marker)) resolve();
      };
    });
    socket.close();
    return echoed;
  }, {
    path: terminalPath,
    cols: PHONE_COLS,
    rows: PHONE_ROWS,
    protocol: TERMINAL_SUBPROTOCOL,
    tag: TERMINAL_TAG_OUTPUT,
    offset: TERMINAL_PAYLOAD_OFFSET,
  });
}

test.use({ trace: "off" });

test("a narrower viewer attaching after a wider one paints the program's frame at its own width", async ({ browser }) => {
  const first = await openViewer(browser, WIDE_VIEWER);
  await startGrid(first);
  const second = await openViewer(browser, NARROW_VIEWER);
  await closeViewer(first);
  await openSession(second);
  await expectGridFrame(second, "narrower viewer after a wider one");
});

test("a wider viewer attaching after a narrower one paints the program's frame at its own width", async ({ browser }) => {
  const first = await openViewer(browser, NARROW_VIEWER);
  await startGrid(first);
  const second = await openViewer(browser, WIDE_VIEWER);
  await closeViewer(first);
  await openSession(second);
  await expectGridFrame(second, "wider viewer after a narrower one");
});

test("a viewer attaching at the size the previous viewer left paints the program's frame", async ({ browser }) => {
  const first = await openViewer(browser, EQUAL_VIEWER);
  await startGrid(first);
  const second = await openViewer(browser, EQUAL_VIEWER);
  await closeViewer(first);
  await openSession(second);
  await expectGridFrame(second, "same-size viewer after another");
});

test("reloading the tab paints the program's frame without a resize", async ({ browser }) => {
  const page = await openViewer(browser, EQUAL_VIEWER);
  await startGrid(page);
  await page.reload();
  await openSession(page);
  await expectGridFrame(page, "the same tab after a reload");
});

test("a desktop viewer attaching after a phone-sized one paints the program's frame at its own width", async ({ browser }) => {
  const first = await openViewer(browser, EQUAL_VIEWER);
  const terminalPath = await startGrid(first);
  const desktop = await openViewer(browser, EQUAL_VIEWER);
  await closeViewer(first);
  const phonePty = await attachAsPhone(desktop, terminalPath);
  expect(phonePty, "the phone attach set the PTY to the phone's size").toEqual({ cols: PHONE_COLS, rows: PHONE_ROWS });
  await openSession(desktop);
  await expectGridFrame(desktop, "desktop viewer after a phone-sized PTY");
});
