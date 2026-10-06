import { defineConfig } from "@playwright/test";

export default defineConfig({
  testDir: "./e2e",
  timeout: 120_000,
  expect: { timeout: 10_000 },
  fullyParallel: true,
  workers: 4,
  reporter: [
    ["line"],
    ["json", { outputFile: "test-results/results.json" }],
    ["./e2e/timingReporter.ts"],
  ],
  globalSetup: "./e2e/global-setup.ts",
  use: {
    headless: true,
    // The suite's colour expectations were written against the dark
    // appearance, so it is pinned here rather than left on Playwright's
    // own default. appearance-light-mode.spec.ts overrides both.
    colorScheme: "dark",
    viewport: { width: 1440, height: 1000 },
    trace: "retain-on-failure",
    screenshot: "only-on-failure",
  },
  projects: [
    {
      name: "functional",
      testIgnore: /terminal-(agent-replay|attach-geometry|board-return|performance|latency)\.spec\.ts/,
    },
    {
      name: "terminal-heavy",
      testMatch: /terminal-(agent-replay|attach-geometry|board-return)\.spec\.ts/,
    },
    {
      name: "performance",
      use: { trace: "off", screenshot: "off" },
      testMatch: /terminal-(performance|latency)\.spec\.ts/,
    },
  ],
});
