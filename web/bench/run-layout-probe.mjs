import { chromium } from "playwright";
import { createServer } from "vite";

const port = 5201;
const server = await createServer({ root: process.cwd(), server: { port, strictPort: true }, logLevel: "error" });
await server.listen();
const browser = await chromium.launch();
const page = await browser.newPage({ viewport: { width: 1440, height: 1000 } });
const errors = [];
page.on("pageerror", (error) => errors.push(String(error)));
try {
  await page.goto(`http://localhost:${port}/layout-probe.html`);
  await page.waitForFunction("window.__layoutProbe", null, { timeout: 180000 });
  const screen = await page.locator('.term-layer[style*="visible"] .xterm-screen').boundingBox();
  if (!screen) throw new Error("missing visible terminal screen");
  await page.mouse.move(screen.x + screen.width / 2, screen.y + screen.height / 2);
  await page.mouse.wheel(0, 130);
  await page.waitForTimeout(100);
  const trustedWheel = await page.evaluate(() => {
    const result = window.__layoutProbe;
    return {
      ...result,
      ...window.__layoutProbeAfterWheel(),
    };
  });
  console.log(JSON.stringify(trustedWheel, null, 2));
  if (trustedWheel.lifecycleRecovery.cols <= 80 || trustedWheel.lifecycleRecovery.rows <= 24) {
    throw new Error("terminal did not recover after its host layout settled");
  }
  if (trustedWheel.claudeState.mode !== "drag" || trustedWheel.claudeState.buffer !== "alternate") {
    throw new Error("Claude fixture did not enter alternate-screen mouse mode");
  }
  if (trustedWheel.codexState.mode !== "none" || trustedWheel.codexState.buffer !== "normal") {
    throw new Error("Codex inherited Claude terminal protocol state");
  }
  if (trustedWheel.failures.length) throw new Error("warm terminal lifecycle assertions failed");
  if (trustedWheel.afterWheel <= trustedWheel.beforeWheel) {
    throw new Error("Codex viewport did not consume trusted wheel input");
  }
  if (trustedWheel.afterInput !== trustedWheel.beforeInput) {
    throw new Error("Codex wheel input was incorrectly forwarded as PTY mouse input");
  }
  const slider = page.locator('.term-layer[style*="visible"] .scrollbar.vertical .slider');
  const sliderBox = await slider.boundingBox();
  if (!sliderBox) throw new Error("missing scrollbar slider");
  await page.mouse.move(sliderBox.x + sliderBox.width / 2, sliderBox.y + sliderBox.height / 2);
  await page.mouse.down();
  await page.mouse.move(sliderBox.x + sliderBox.width / 2, sliderBox.y + sliderBox.height + 300, { steps: 8 });
  await page.mouse.up();
  await page.waitForTimeout(100);
  const afterDrag = await page.evaluate(() => window.__layoutProbeAfterWheel());
  if (afterDrag.afterWheel <= trustedWheel.afterWheel) {
    throw new Error("Codex viewport did not follow scrollbar drag");
  }
  if (afterDrag.afterInput !== trustedWheel.beforeInput) {
    throw new Error("Codex scrollbar drag was incorrectly forwarded as PTY mouse input");
  }
  await page.locator('.term-layer[style*="visible"] .xterm-helper-textarea').focus();
  await page.keyboard.press("Shift+PageUp");
  await page.waitForTimeout(100);
  const afterPageUp = await page.evaluate(() => window.__layoutProbeAfterWheel());
  if (afterPageUp.afterWheel >= afterDrag.afterWheel) {
    throw new Error("Codex viewport did not consume keyboard Page Up");
  }
  if (afterPageUp.afterInput !== trustedWheel.beforeInput) {
    throw new Error("Codex keyboard scroll was incorrectly forwarded as PTY input");
  }
} finally {
  if (errors.length) console.error(errors.join("\n"));
  await browser.close();
  await server.close();
}
