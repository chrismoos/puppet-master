import { expect, test, type Page } from "./fixtures";
import { expectTerminalRevealed, logIn } from "./support";

/** Rejoins wrapped rows from the visible terminal snapshot. */
async function terminalText(page: Page): Promise<string> {
  return page.evaluate(() => {
    const stage = (window as unknown as {
      __pmStage?: {
        debugSnapshot(): Array<{ key: string; visible: boolean }>;
        layers: Map<string, { term: { renderSnapshot(): { rowData: Array<{ text: string; isWrapContinuation: boolean }> } | null } }>;
      };
    }).__pmStage;
    const visible = stage?.debugSnapshot().find((entry) => entry.visible);
    const term = visible ? stage?.layers.get(visible.key)?.term : undefined;
    if (!term) return "";
    const rows = term.renderSnapshot()?.rowData ?? [];
    let text = "";
    for (const [index, line] of rows.entries()) {
      text += `${index > 0 && line.isWrapContinuation ? "" : "\n"}${line.text}`;
    }
    return text;
  });
}

async function fileDrag(page: Page, name: string, body: string) {
  return page.evaluateHandle(
    ([name, body]) => {
      const transfer = new DataTransfer();
      transfer.items.add(new File([body], name, { type: "image/png" }));
      return transfer;
    },
    [name, body] as const,
  );
}

test("a file dropped on a session's terminal is stored on its host and its path typed in", async ({ page }) => {
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await expectTerminalRevealed(page, "s:");
  const pane = page.locator(".term-main");
  const dataTransfer = await fileDrag(page, "Screen Shot.png", "png-bytes");

  await pane.dispatchEvent("dragover", { dataTransfer });
  await expect(page.locator(".term-drop-overlay.is-over")).toBeVisible();

  await pane.dispatchEvent("drop", { dataTransfer });
  // The overlay clears only once the file is stored and its path typed.
  await expect(page.locator(".term-drop-overlay")).toHaveCount(0);

  await expect
    .poll(() => terminalText(page), { message: "waiting for the dropped path in the composer" })
    .toMatch(/session-\d+\/Screen-Shot\.png/);
  const text = await terminalText(page);
  expect(text).not.toContain("Screen Shot.png");
});

test("a folder dropped on a terminal is refused rather than uploaded", async ({ page }) => {
  await logIn(page);
  await page.locator(".sb-session").first().click();
  await expectTerminalRevealed(page, "s:");
  const pane = page.locator(".term-main");
  // A dropped directory arrives as an entry that reports itself as one.
  const dataTransfer = await page.evaluateHandle(() => {
    const transfer = new DataTransfer();
    transfer.items.add(new File([], "screenshots", { type: "" }));
    return transfer;
  });

  await pane.dispatchEvent("drop", { dataTransfer });
  await expect(page.locator(".term-drop-overlay.is-failed")).toContainText("folder");
});

test("a file dropped outside a terminal does not navigate the dashboard away", async ({ page }) => {
  await logIn(page);
  const before = page.url();
  const dataTransfer = await fileDrag(page, "stray.png", "bytes");

  // Without the guard the browser opens the file and the dashboard, with
  // every terminal in it, is gone.
  const prevented = await page.locator(".sb-session").first().evaluate((element, transfer) => {
    const event = new DragEvent("drop", { dataTransfer: transfer as DataTransfer, bubbles: true, cancelable: true });
    element.dispatchEvent(event);
    return event.defaultPrevented;
  }, dataTransfer);

  expect(prevented).toBe(true);
  expect(page.url()).toBe(before);
});
