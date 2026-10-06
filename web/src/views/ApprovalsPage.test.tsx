// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { renderToStaticMarkup } from "react-dom/server";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import type { ApprovalDetail, ApprovalSummary } from "@puppet-master/client-core/approvals";
import * as api from "../api/approvals";
import * as router from "../router";
import { ApprovalsButton, ApprovalsPage } from "./ApprovalsPage";

vi.mock("../api/approvals", () => ({
  listApprovals: vi.fn(),
  getApproval: vi.fn(),
  decideApproval: vi.fn(),
}));
vi.mock("../router", () => ({ navigate: vi.fn() }));

const NOW = Date.UTC(2026, 9, 2, 12, 0, 0);

function summary(id: string, minutesAgo: number, status = "pending"): ApprovalSummary {
  return {
    id,
    session_id: 7,
    project_id: 2,
    connection_id: 3,
    connection_revision: 4,
    tool: `tool_${id}`,
    justification: `Because ${id}`,
    status,
    created_at: NOW - minutesAgo * 60_000,
    expires_at: NOW + 60 * 60_000,
    decided_by: status === "pending" ? null : "testuser",
    error: null,
    project_name: "payments",
    session_name: "refund worker",
    session_state: "working",
    session_role: "worker",
    session_live: true,
    connection_name: "Billing API",
    connection_endpoint: "https://billing.example.com/v1",
    connection_kind: "openapi",
    connection_active: true,
    connection_current: true,
    tool_access: "write",
    tool_description: "Refund a payment",
  };
}

function detail(base: ApprovalSummary, args: unknown): ApprovalDetail {
  return { ...base, request_id: "r", arguments: args, result: null };
}

let root: Root;
let host: HTMLDivElement;
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  vi.useFakeTimers({ now: NOW, toFake: ["Date"] });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.useRealTimers();
  vi.clearAllMocks();
});

async function render(id?: string) {
  await act(async () => root.render(<ApprovalsPage id={id} />));
  await act(async () => {});
}

const button = (label: string) =>
  [...document.querySelectorAll<HTMLButtonElement>(".approval-modal button")].find((b) => b.textContent === label)!;

it("labels the top bar entry and counts what waits", () => {
  expect(renderToStaticMarkup(<ApprovalsButton pending={0} />)).toContain('aria-label="Approvals"');
  const waiting = renderToStaticMarkup(<ApprovalsButton pending={3} />);
  expect(waiting).toContain('aria-label="Approvals, 3 waiting"');
  expect(waiting).toContain('href="#/approvals"');
  expect(waiting).toContain(">3</span>");
  expect(renderToStaticMarkup(<ApprovalsButton pending={120} />)).toContain(">99+</span>");
});

it("lists approvals newest first with status and context, whatever order arrives", async () => {
  vi.mocked(api.listApprovals).mockResolvedValue([
    summary("older", 90, "denied"),
    summary("newest", 2),
    summary("middle", 30, "succeeded"),
  ]);
  await render();
  const rows = [...host.querySelectorAll(".approval-row")];
  expect(rows.map((row) => row.querySelector(".approval-tool")?.textContent)).toEqual([
    "tool_newest",
    "tool_middle",
    "tool_older",
  ]);
  expect(rows[0].textContent).toContain("Waiting for you");
  expect(rows[0].textContent).toContain("Billing API · payments");
  expect(rows[0].textContent).toContain("2 min ago");
  expect(rows[0].getAttribute("href")).toBe("#/approvals/newest");
  expect(rows[2].textContent).toContain("Denied");
  expect(host.textContent).toContain("1 waiting.");
  expect(host.querySelector("[role=dialog]")).toBeNull();
});

it("shows an empty state and a retryable error state", async () => {
  vi.mocked(api.listApprovals).mockRejectedValueOnce(new Error("controller unreachable"));
  await render();
  expect(host.querySelector("[role=alert]")?.textContent).toContain("controller unreachable");
  vi.mocked(api.listApprovals).mockResolvedValue([]);
  await act(async () => host.querySelector<HTMLButtonElement>(".approvals-error button")!.click());
  await act(async () => {});
  expect(host.querySelector("[role=alert]")).toBeNull();
  expect(host.textContent).toContain("No approvals yet");
});

it("opens the routed approval in a modal with its exact arguments and approves it once", async () => {
  const pending = summary("abc", 5);
  const args = { path: { id: "order-17" }, body: { name: "Reviewed" } };
  vi.mocked(api.listApprovals).mockResolvedValue([pending]);
  vi.mocked(api.getApproval).mockResolvedValue(detail(pending, args));
  vi.mocked(api.decideApproval).mockResolvedValue({ status: "authorized" });
  await render("abc");
  expect(api.getApproval).toHaveBeenCalledWith("abc");
  const dialog = host.querySelector("[role=dialog]")!;
  expect(dialog.getAttribute("aria-modal")).toBe("true");
  expect(dialog.querySelector("h2")?.textContent).toBe("tool_abc");
  expect(dialog.querySelector(".approval-arguments pre")?.textContent).toBe(JSON.stringify(args, null, 2));
  for (const fact of ["payments", "refund worker", "Billing API", "https://billing.example.com/v1", "Because abc", "Refund a payment"])
    expect(dialog.textContent).toContain(fact);
  expect(host.querySelector(".approval-row")?.getAttribute("aria-current")).toBe("true");
  await act(async () => button("Approve and execute").click());
  await act(async () => {});
  expect(api.decideApproval).toHaveBeenCalledTimes(1);
  expect(api.decideApproval).toHaveBeenCalledWith("abc", true);
  expect(dialog.querySelector(".approvals-outcome")?.textContent).toContain("Running once with the arguments shown");
});

it("rejects without invoking, and says so when a decision lost the race", async () => {
  const pending = summary("abc", 5);
  vi.mocked(api.listApprovals).mockResolvedValue([pending]);
  vi.mocked(api.getApproval).mockResolvedValue(detail(pending, {}));
  vi.mocked(api.decideApproval).mockResolvedValue({ status: "expired" });
  await render("abc");
  await act(async () => button("Reject").click());
  await act(async () => {});
  expect(api.decideApproval).toHaveBeenCalledWith("abc", false);
  expect(host.querySelector("[role=dialog] [role=alert]")?.textContent).toContain("already expired");
});

it("closes back to the list on Escape and on the close button", async () => {
  vi.mocked(api.listApprovals).mockResolvedValue([summary("abc", 5)]);
  vi.mocked(api.getApproval).mockResolvedValue(detail(summary("abc", 5), {}));
  await render("abc");
  const dialog = host.querySelector<HTMLElement>("[role=dialog]")!;
  expect(document.activeElement?.getAttribute("aria-label")).toBe("Close approval");
  await act(async () => dialog.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape", bubbles: true })));
  expect(router.navigate).toHaveBeenCalledWith("/approvals");
  await act(async () => host.querySelector<HTMLButtonElement>(".approval-modal-close")!.click());
  expect(router.navigate).toHaveBeenCalledTimes(2);
});

it("offers no actions on a decided approval and warns before an approval that cannot run", async () => {
  const decided = summary("done", 5, "denied");
  vi.mocked(api.listApprovals).mockResolvedValue([decided]);
  vi.mocked(api.getApproval).mockResolvedValue(detail(decided, {}));
  await render("done");
  expect(host.querySelector(".approval-modal-actions")).toBeNull();
  expect(host.textContent).toContain("Decided by");

  const stale = { ...summary("stale", 5), connection_current: false };
  vi.mocked(api.getApproval).mockResolvedValue(detail(stale, {}));
  await render("stale");
  expect(host.querySelector(".approvals-warning")?.textContent).toContain("connection changed");
});

it("reports an approval that no longer exists", async () => {
  vi.mocked(api.listApprovals).mockResolvedValue([]);
  vi.mocked(api.getApproval).mockRejectedValue(new Error("This approval no longer exists"));
  await render("gone");
  expect(host.querySelector("[role=dialog] [role=alert]")?.textContent).toContain("no longer exists");
});
