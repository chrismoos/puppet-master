import type { BrowserContext, Page, Request } from "@playwright/test";
import { expect, test } from "./fixtures";
import { logIn } from "./support";

/** The board names plans with links, so one opens in its own window and
 * the board the reader was on stays put. */
async function openPlanFromBoard(
  page: Page,
  context: BrowserContext,
  name: RegExp,
): Promise<Page> {
  await page.goto(`${process.env.PM_E2E_BASE_URL}/#/bucket/1/board`);
  const chip = page.getByRole("region", { name: "bucket plans" }).getByRole("link", { name });
  const [opened] = await Promise.all([context.waitForEvent("page"), chip.click()]);
  return opened;
}

test("an agent plan is visible in its bucket and supports a live custom decision", async ({
  page,
  context,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const created = JSON.parse(await isolatedDaemon.callAgentTool("browser-e2e", "upsert_plan", {
    name: "Service architecture",
    summary: "Agree on the durable system shape",
    markdown_path: "docs/service-plan.md",
  })) as { id: number };
  await isolatedDaemon.callAgentTool("browser-e2e", "present_plan_decision", {
    plan: created.id,
    key: "store",
    title: "Choose the primary store",
    prompt_markdown: "Pick the default that best fits the deployment.",
    detail_markdown: "This choice drives the next planning question.",
    mode: "single",
    allow_custom: true,
    options: [
      { key: "postgres", label: "Postgres", detail_markdown: "Strong relational guarantees." },
      { key: "sqlite", label: "SQLite", detail_markdown: "Simple single-host operation." },
    ],
  });

  await logIn(page);
  await page.goto(`${process.env.PM_E2E_BASE_URL}/#/bucket/1/board`);
  const shelf = page.getByRole("region", { name: "bucket plans" });
  await expect(shelf.getByRole("link", { name: /Service architecture/ })).toContainText("decision ready");

  const plan = await openPlanFromBoard(page, context, /Service architecture/);

  await expect(plan.getByRole("heading", { name: "Choose the primary store" })).toBeVisible();
  await plan.getByRole("button", { name: /SQLite/ }).click();
  await expect(plan.locator(".plan-option-detail")).toContainText("Simple single-host operation.");

  await plan.getByRole("radio", { name: "Add your own option" }).click();
  await plan.getByPlaceholder("Describe your option…").fill("Managed database\n\nLet the platform own backups and failover.");
  const custom = plan.locator(".plan-custom-option");
  await expect(custom).toHaveClass(/is-selected/);
  await plan.getByRole("button", { name: /SUBMIT DECISION/ }).click();

  await expect(plan.getByText("Waiting for agent")).toBeVisible();
  await expect(plan.getByText("Answers submitted. Dialogue remains open.")).toBeVisible();
  const composer = plan.getByPlaceholder(/Message the agent…/);
  await composer.fill("Please compare operating costs too.");
  await composer.press("Enter");
  await expect(plan.locator(".plan-message.is-user", { hasText: "Please compare operating costs too." })).toBeVisible();

  // The board the reader opened it from never moved.
  await expect(page).toHaveURL(/#\/bucket\/1\/board$/);
  await plan.close();
});

test("independent decisions keep their answers and submit once at the end", async ({
  page,
  context,
  isolatedDaemon,
}) => {
  const created = JSON.parse(await isolatedDaemon.callAgentTool("browser-e2e", "upsert_plan", {
    name: "Release choices",
    markdown_path: "docs/release-plan.md",
  })) as { id: number };
  await isolatedDaemon.callAgentTool("browser-e2e", "present_plan_decision_batch", {
    plan: created.id,
    batch_key: "release-defaults",
    decisions: [
      {
        key: "region",
        title: "Choose a region",
        mode: "single",
        options: [
          { key: "east", label: "East" },
          { key: "west", label: "West" },
        ],
      },
      {
        key: "notifications",
        title: "Choose notifications",
        mode: "multiple",
        options: [
          { key: "email", label: "Email" },
          { key: "push", label: "Push" },
        ],
      },
    ],
  });
  let submissions = 0;
  // The plan answers from its own window, so the count has to watch the
  // whole context rather than the page the board is on.
  context.on("request", (request: Request) => {
    if (request.method() === "POST" && request.url().endsWith(`/api/plans/${created.id}/decisions/respond`)) {
      submissions += 1;
    }
  });

  await logIn(page);
  const plan = await openPlanFromBoard(page, context, /Release choices/);
  await expect(plan.getByRole("heading", { name: "Choose a region" })).toBeVisible();
  await plan.getByRole("button", { name: /West/ }).click();
  await plan.getByRole("button", { name: "Next" }).click();
  expect(submissions).toBe(0);

  await plan.getByRole("button", { name: /Email/ }).click();
  await plan.getByRole("button", { name: "Back" }).click();
  await expect(plan.getByRole("button", { name: /West/ })).toHaveClass(/is-selected/);
  await plan.getByRole("button", { name: "Next" }).click();
  await plan.getByRole("button", { name: /SUBMIT ALL/ }).click();

  await expect(plan.getByText("Waiting for agent")).toBeVisible();
  expect(submissions).toBe(1);
  await plan.close();
});

test("plan context rail toggles markdown takeover in center stage and cleans decision labels", async ({
  page,
  context,
  isolatedDaemon,
}) => {
  const session = isolatedDaemon.session("browser-e2e");
  expect(session).not.toBeNull();
  const created = JSON.parse(await isolatedDaemon.callAgentTool("browser-e2e", "upsert_plan", {
    name: "Context rail plan",
    summary: "Testing plan context rail",
    markdown_path: "docs/context-rail.md",
  })) as { id: number };
  await isolatedDaemon.callAgentTool("browser-e2e", "present_plan_decision", {
    plan: created.id,
    key: "choice",
    title: "Configure persistence",
    prompt_markdown: "Select the backend",
    mode: "single",
    options: [
      { key: "opt1", label: "Option 1", detail_markdown: "Detail 1" },
    ],
  });

  await logIn(page);
  const plan = await openPlanFromBoard(page, context, /Context rail plan/);

  await expect(plan.getByRole("heading", { name: "Configure persistence" })).toBeVisible();
  await expect(plan.getByText("FOCUSED DECISION")).toHaveCount(0);
  await expect(plan.getByText("YOUR DECISION")).toHaveCount(0);

  await plan.locator(".plan-rail-btn").click();
  await expect(plan.locator(".plan-context-view")).toBeVisible();
  await expect(plan.getByRole("heading", { name: "Configure persistence" })).toBeHidden();

  await plan.locator(".plan-return-btn").click();
  await expect(plan.getByRole("heading", { name: "Configure persistence" })).toBeVisible();
  await expect(plan.locator(".plan-context-view")).toBeHidden();
  await plan.close();
});

test("a batch remembers the focused decision and jumps between decisions", async ({
  page,
  context,
  isolatedDaemon,
}) => {
  const created = JSON.parse(await isolatedDaemon.callAgentTool("browser-e2e", "upsert_plan", {
    name: "Rollout choices",
    markdown_path: "docs/rollout-plan.md",
  })) as { id: number };
  await isolatedDaemon.callAgentTool("browser-e2e", "present_plan_decision_batch", {
    plan: created.id,
    batch_key: "rollout-defaults",
    decisions: [
      { key: "region", title: "Choose a region", mode: "single", options: [{ key: "east", label: "East" }, { key: "west", label: "West" }] },
      { key: "window", title: "Choose a window", mode: "single", options: [{ key: "day", label: "Daytime" }, { key: "night", label: "Overnight" }] },
      { key: "channel", title: "Choose a channel", mode: "single", options: [{ key: "slack", label: "Slack" }, { key: "mail", label: "Mail" }] },
    ],
  });

  await logIn(page);
  const plan = await openPlanFromBoard(page, context, /Rollout choices/);
  await expect(plan.getByRole("heading", { name: "Choose a region" })).toBeVisible();
  await plan.getByRole("button", { name: /West/ }).click();

  const steps = plan.getByRole("navigation", { name: "Decisions in this batch" });
  await steps.getByRole("button", { name: "Decision 3: Choose a channel" }).click();
  await expect(plan.getByRole("heading", { name: "Choose a channel" })).toBeVisible();
  await expect(plan.getByText("Decision 3 of 3")).toBeVisible();
  await expect(steps.getByRole("button", { name: "Decision 1: Choose a region (answered)" })).toBeVisible();
  await expect(steps.getByRole("button", { name: "Decision 3: Choose a channel" })).toHaveAttribute("aria-current", "step");

  // The window carries the plan's own address, so a reload lands back on
  // the decision the reader was answering.
  await plan.reload();
  await expect(plan.getByRole("heading", { name: "Choose a channel" })).toBeVisible();
  await expect(plan.getByText("Decision 3 of 3")).toBeVisible();

  await steps.getByRole("button", { name: "Decision 2: Choose a window" }).click();
  await expect(plan.getByRole("heading", { name: "Choose a window" })).toBeVisible();
  await expect(plan.getByRole("button", { name: "Back" })).toBeEnabled();
  await plan.close();
});
