import { createServer } from "node:http";
import { mkdir, writeFile } from "node:fs/promises";
import { join } from "node:path";
import { expect, test } from "./fixtures";
import { apiHeaders, logIn } from "./support";

const TOOL_COUNT = 125;
const PAGE_ROWS = 25;
const PANEL_DRAG_PX = 120;
const PHONE_WIDTH = 390;
const PHONE_HEIGHT = 844;

for (const narrow of [false, true]) {
  test(`agent-assisted REST setup, filtered policy review, and privileged approval ${narrow ? "in drawer" : "beside session"}`, async ({
    page,
    isolatedDaemon,
  }) => {
    if (narrow)
      await page.setViewportSize({ width: PHONE_WIDTH, height: PHONE_HEIGHT });
    let writes = 0;
    const upstream = createServer((request, response) => {
      if (request.headers.authorization !== "Bearer placeholder-credential") {
        response.writeHead(401);
        response.end();
        return;
      }
      if (request.method === "PUT") writes++;
      response.setHeader("Content-Type", "application/json");
      response.end(JSON.stringify({ ok: true }));
    });
    await new Promise<void>((resolve) =>
      upstream.listen(0, "127.0.0.1", resolve),
    );
    try {
      const address = upstream.address();
      if (!address || typeof address === "string")
        throw new Error("Missing upstream address");
      const endpoint = `http://127.0.0.1:${address.port}`;
      const paths = Object.fromEntries(
        Array.from({ length: TOOL_COUNT }, (_, index) => [
          `/records/${index}`,
          {
            get: {
              operationId: `record${String(index).padStart(3, "0")}`,
              summary: "Read a record",
            },
          },
        ]),
      );
      paths["/orders"] = {
        put: { operationId: "update_order", summary: "Update an order" },
      } as unknown as (typeof paths)[string];
      await logIn(page);
      await page
        .locator(".sb-session")
        .filter({
          has: page
            .locator(".sb-session-title")
            .getByText("browser-e2e", { exact: true }),
        })
        .click();
      // An agent hands over its document as a file in its working directory.
      const cwd = isolatedDaemon.session("browser-e2e")!.cwd;
      await writeFile(
        join(cwd, "orders-openapi.json"),
        JSON.stringify({ openapi: "3.1.0", paths }),
      );
      const draft = JSON.parse(
        await isolatedDaemon.callAgentTool("browser-e2e", "seed_connection", {
          name: "Orders API",
          kind: "openapi",
          endpoint,
          schema_path: "orders-openapi.json",
        }),
      );
      const panel = page.getByRole("complementary", {
        name: "Session connections",
      });
      await expect(
        panel.getByRole("heading", { name: "Orders API" }),
      ).toBeVisible();
      // A draft that has not been tested opens on its credentials.
      await expect(
        panel.getByRole("tab", { name: "Credentials" }),
      ).toHaveAttribute("aria-selected", "true");
      if (!narrow) {
        const handle = panel.getByRole("separator", {
          name: "resize connection panel",
        });
        const before = (await panel.boundingBox())!;
        const grip = (await handle.boundingBox())!;
        await page.mouse.move(grip.x + grip.width / 2, grip.y + 200);
        await page.mouse.down();
        await page.mouse.move(before.x - PANEL_DRAG_PX, grip.y + 200, {
          steps: 4,
        });
        await page.mouse.up();
        await expect
          .poll(async () => (await panel.boundingBox())!.width)
          .toBeGreaterThan(before.width + PANEL_DRAG_PX / 2);
        expect(
          await page.evaluate(() =>
            Number(localStorage.getItem("pm.connectionPanelWidth")),
          ),
        ).toBeGreaterThan(before.width);
      }
      await panel
        .getByLabel("Authentication", { exact: true })
        .selectOption("bearer");
      await panel
        .getByLabel("Token", { exact: true })
        .fill("placeholder-credential");
      await panel
        .getByRole("button", { name: "Save draft", exact: true })
        .click();
      await expect(panel.getByRole("status")).toHaveText(
        "Draft saved. Test it before activation.",
      );
      if (!narrow && process.env.PM_CONNECTION_SCREENSHOT_DIR) {
        await mkdir(process.env.PM_CONNECTION_SCREENSHOT_DIR, {
          recursive: true,
        });
        await page.screenshot({
          path: join(
            process.env.PM_CONNECTION_SCREENSHOT_DIR,
            "actual-setup.png",
          ),
        });
      }
      await panel
        .getByRole("button", { name: "Test connection", exact: true })
        .click();
      await expect(panel.getByRole("status")).toContainText("Schema imported");
      const headers = await apiHeaders(page);
      const base = process.env.PM_E2E_BASE_URL!;
      const full = await (
        await page.request.get(`${base}/api/connections/${draft.id}`, {
          headers,
        })
      ).json();
      const rules = Object.fromEntries(
        full.tools.map((tool: { name: string }) => [
          tool.name,
          { access: tool.name === "update_order" ? "write" : "read" },
        ]),
      );
      await writeFile(
        join(cwd, "orders-policy.json"),
        JSON.stringify({
          read_policy: "allow",
          write_policy: "approve",
          unknown_policy: "approve",
          rules,
        }),
      );
      await isolatedDaemon.callAgentTool(
        "browser-e2e",
        "propose_connection_policy",
        {
          connection_id: draft.id,
          revision: full.revision,
          policy_path: "orders-policy.json",
          explanation: "Always allow reads and ask before changing orders.",
        },
      );
      // The proposal brings the policy tab forward on its own.
      const proposal = panel.getByRole("region", {
        name: "Proposed connection policy",
      });
      await expect(proposal).toBeVisible();
      await expect(
        panel.getByRole("tab", { name: "Policy" }),
      ).toHaveAttribute("aria-selected", "true");
      await expect(proposal.locator("tbody tr")).toHaveCount(PAGE_ROWS);
      await proposal
        .getByLabel("Filter proposed policy tools")
        .fill("record124");
      await expect(proposal.locator("tbody tr")).toHaveCount(1);
      await expect(proposal).toContainText("record124");
      await panel.getByLabel("Filter connection tools").fill("record124");
      await expect(panel.locator(".connection-tool")).toHaveCount(1);
      await panel.getByLabel("Filter connection tools").fill("");
      await panel.evaluate((element) => {
        element.scrollTop = 0;
      });
      if (process.env.PM_CONNECTION_SCREENSHOT_DIR) {
        await page.screenshot({
          path: join(
            process.env.PM_CONNECTION_SCREENSHOT_DIR,
            narrow ? "actual-drawer.png" : "actual-policy.png",
          ),
        });
      }
      await proposal
        .getByRole("button", { name: "Apply proposed policy", exact: true })
        .click();
      await expect(panel.getByRole("status")).toHaveText("Policy applied.");
      await panel
        .getByRole("button", { name: "Activate", exact: true })
        .click();
      // Activation completes setup, so the whole sidebar closes.
      await expect(panel).toBeHidden();
      // The call holds for its result, so the approval below lands inside it.
      const held = isolatedDaemon.callAgentTool(
        "browser-e2e",
        "call_connection_tool",
        {
          connection_id: draft.id,
          tool: "update_order",
          arguments: {},
          request_id: "browser-write",
          justification: "Update the reviewed order",
        },
      );
      const approval = panel.locator(".connection-approval");
      await expect(approval).toContainText("Orders API");
      await expect(approval).toContainText("Update the reviewed order");
      await approval.getByText("Exact arguments", { exact: true }).click();
      await expect(approval.locator("pre")).toHaveText("{}");
      expect(writes).toBe(0);
      await approval
        .getByRole("button", { name: "Approve and execute" })
        .click();
      const call = JSON.parse(await held);
      expect(call.status).toBe("succeeded");
      expect(writes).toBe(1);
      expect(
        JSON.parse(
          await isolatedDaemon.callAgentTool(
            "browser-e2e",
            "get_connection_call",
            { call_id: call.id },
          ),
        ).status,
      ).toBe("succeeded");
      // Connections is a Settings page, at the address it has always had.
      await page.goto(`${base}/#/settings/connections`);
      const settings = page.getByRole("region", { name: "connections" });
      await settings.getByLabel("Filter connections").fill("Orders");
      const listed = settings
        .locator("table")
        .first()
        .locator("tr.cx-row-link", { hasText: "Orders API" });
      await expect(listed).toContainText("active");
      await settings.getByLabel("Filter connection calls").fill("update_order");
      const row = settings.locator("tr.cx-row-link", {
        hasText: "update_order",
      });
      await expect(row).toHaveCount(1);
      await expect(row).toContainText("succeeded");

      // A call opens over the list at an address of its own.
      await row.locator("td.cx-name").click();
      await expect(page).toHaveURL(
        new RegExp(`#/settings/connections/calls/${call.id}$`),
      );
      const details = page.getByRole("dialog");
      await expect(details).toContainText("Update the reviewed order");
      await expect(details).toContainText("Approved by");
      await expect(details.locator("pre").first()).toHaveText("{}");
      await page.reload();
      await expect(page.getByRole("dialog")).toContainText("update_order");
      await page
        .getByRole("dialog")
        .getByRole("button", { name: "Close", exact: true })
        .click();
      await expect(page).toHaveURL(/#\/settings\/connections$/);
      await expect(page.getByRole("dialog")).toBeHidden();

      // A connection has a page of its own, apart from the list.
      await settings
        .getByRole("link", { name: "Orders API", exact: true })
        .click();
      await expect(page).toHaveURL(
        new RegExp(`#/settings/connections/${draft.id}$`),
      );
      await expect(
        settings.getByRole("heading", { name: "Tools and policy" }),
      ).toBeVisible();
      await expect(settings.locator(".ui-pager").first()).toContainText(
        `1–${PAGE_ROWS} of ${TOOL_COUNT + 1}`,
      );
      await settings.getByLabel("Filter tools").fill("record124");
      await expect(
        settings.getByLabel("Classification for record124"),
      ).toHaveValue("read");
      await expect(settings.locator(".ui-pager").first()).toContainText(
        "1–1 of 1",
      );
      await settings.getByRole("link", { name: "Edit setup" }).click();
      await expect(page).toHaveURL(
        new RegExp(`#/settings/connections/${draft.id}/setup$`),
      );
      await expect(
        settings.getByRole("heading", { name: "Set up Orders API" }),
      ).toBeVisible();
      await expect(settings.getByText("Ask the agent")).toHaveCount(0);
    } finally {
      await new Promise<void>((resolve, reject) =>
        upstream.close((error) => (error ? reject(error) : resolve())),
      );
    }
  });
}

test("an agent's update to an active connection is reviewed and applied in the session panel", async ({
  page,
  isolatedDaemon,
}) => {
  const upstream = createServer((_request, response) => {
    response.setHeader("Content-Type", "application/json");
    response.end(JSON.stringify({ ok: true }));
  });
  await new Promise<void>((resolve) => upstream.listen(0, "127.0.0.1", resolve));
  try {
    const address = upstream.address();
    if (!address || typeof address === "string") throw new Error("Missing upstream address");
    const operation = (operationId: string) => ({ operationId, summary: operationId });
    const paths = { "/orders": { get: operation("list_orders") } };
    await logIn(page);
    const headers = await apiHeaders(page);
    const base = process.env.PM_E2E_BASE_URL!;
    const post = async (path: string, data: unknown) => {
      const response = await page.request.post(`${base}${path}`, { headers, data });
      expect(response.ok(), await response.text()).toBe(true);
      return response.json();
    };
    const agent = async (name: string, args: Record<string, unknown>) =>
      JSON.parse(await isolatedDaemon.callAgentTool("browser-e2e", name, args));
    const cwd = isolatedDaemon.session("browser-e2e")!.cwd;
    await writeFile(
      join(cwd, "orders-openapi.json"),
      JSON.stringify({ openapi: "3.1.0", paths }),
    );
    const draft = await agent("seed_connection", {
      name: "Orders API",
      kind: "openapi",
      endpoint: `http://127.0.0.1:${address.port}`,
      schema_path: "orders-openapi.json",
    });
    const tested = await post(`/api/connections/${draft.id}/test`, {});
    await writeFile(
      join(cwd, "orders-policy.json"),
      JSON.stringify({ rules: { list_orders: { access: "read" } } }),
    );
    const policy = await agent("propose_connection_policy", {
      connection_id: draft.id,
      revision: tested.revision,
      policy_path: "orders-policy.json",
      explanation: "Reads are safe.",
    });
    const classified = await post(`/api/connections/${draft.id}/policy`, {
      revision: tested.revision,
      proposal_id: policy.proposal.id,
      accept: true,
    });
    const active = await post(`/api/connections/${draft.id}/active`, {
      revision: classified.revision,
      active: true,
    });

    await page
      .locator(".sb-session")
      .filter({
        has: page.locator(".sb-session-title").getByText("browser-e2e", { exact: true }),
      })
      .click();
    const panel = page.getByRole("complementary", { name: "Session connections" });
    // An active connection with nothing to review keeps the panel closed.
    await expect(panel).toBeHidden();

    await writeFile(
      join(cwd, "orders-openapi-v2.json"),
      JSON.stringify({
        openapi: "3.1.0",
        paths: { ...paths, "/orders/{id}": { get: operation("get_order") } },
      }),
    );
    const proposed = await agent("propose_connection_update", {
      connection_id: draft.id,
      revision: active.revision,
      config: { name: "Orders API v2" },
      schema_path: "orders-openapi-v2.json",
      explanation: "Adds the order lookup.",
    });
    expect(proposed.status).toBe("pending_user_confirmation");
    const review = panel.getByRole("region", { name: "Proposed connection policy" });
    await expect(review).toBeVisible();
    await expect(review).toContainText("Agent update proposal");
    await expect(review).toContainText("Adds the order lookup.");
    const changes = review.getByLabel("Proposed connection changes");
    await expect(changes).toContainText("Orders API v2");
    await expect(changes.getByLabel("Proposed tool changes").locator("li")).toHaveText([
      /Added\s*get_order/,
    ]);
    await expect(review.getByRole("button", { name: "Edit proposed policy" })).toHaveCount(0);

    await review.getByRole("button", { name: "Apply proposed update", exact: true }).click();
    // The connection stayed active, so applying finishes the review and the panel closes.
    await expect(panel).toBeHidden();
    const applied = await (
      await page.request.get(`${base}/api/connections/${draft.id}`, { headers })
    ).json();
    expect(applied.active).toBe(true);
    expect(applied.config.name).toBe("Orders API v2");
    expect(applied.tools.map((tool: { name: string }) => tool.name).sort()).toEqual([
      "get_order",
      "list_orders",
    ]);
    expect(applied.config.rules).toEqual({ list_orders: { access: "read", policy: null } });
    const read = await agent("call_connection_tool", {
      connection_id: draft.id,
      tool: "list_orders",
      arguments: {},
      request_id: "read-after-update",
    });
    expect(read.status).toBe("succeeded");
  } finally {
    await new Promise<void>((resolve, reject) =>
      upstream.close((error) => (error ? reject(error) : resolve())),
    );
  }
});
