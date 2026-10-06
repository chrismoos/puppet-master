import { chromium } from "playwright";
import { createServer } from "vite";

const PORT = 5200;
const server = await createServer({
  root: process.cwd(),
  server: { port: PORT, strictPort: true },
  logLevel: "error",
});
await server.listen();

const browser = await chromium.launch();
const page = await browser.newPage();
const errors = [];
page.on("pageerror", (error) => errors.push(String(error)));
page.on("console", (message) => {
  if (message.type() === "error") errors.push(message.text());
});

try {
  await page.goto(`http://localhost:${PORT}/terminal-switch-bench.html`);
  await page.waitForFunction("window.__terminalSwitchResult", null, { timeout: 180000 });
  const result = await page.evaluate("window.__terminalSwitchResult");
  console.log(JSON.stringify(result, null, 2));
} finally {
  if (errors.length) console.error(`PAGE ERRORS:\n${errors.join("\n")}`);
  await browser.close();
  await server.close();
}
