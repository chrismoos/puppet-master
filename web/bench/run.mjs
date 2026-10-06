// Runs the terminal latency probe in real chromium via Playwright over a
// vite dev server, prints the JSON result, and tears everything down.
// Usage: node bench/run.mjs   (from web/)
import { chromium } from "playwright";
import { createServer } from "vite";

const PORT = 5199;
const server = await createServer({
  root: process.cwd(),
  server: { port: PORT, strictPort: true },
  logLevel: "error",
});
await server.listen();

const browser = await chromium.launch();
const page = await browser.newPage();
const errors = [];
page.on("pageerror", (e) => errors.push(String(e)));
page.on("console", (m) => {
  if (m.type() === "error") errors.push(m.text());
});

try {
  await page.goto(`http://localhost:${PORT}/latency-bench.html`);
  await page.waitForFunction("window.__latencyResult", null, { timeout: 180000 });
  const result = await page.evaluate("window.__latencyResult");
  console.log(JSON.stringify(result, null, 2));
} finally {
  if (errors.length) console.error("PAGE ERRORS:\n" + errors.join("\n"));
  await browser.close();
  await server.close();
}
