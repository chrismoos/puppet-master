// @vitest-environment jsdom
import { create } from "@bufbuild/protobuf";
import { describe, expect, it, vi } from "vitest";
import { SessionSchema, TerminalSchema, WorkerSchema } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { initialState, type AppState } from "@puppet-master/client-core/state/reducer";
import {
  renderTerminalStatus,
  terminalRendererFailureView,
  terminalStatusView,
  terminalStatusWorker,
} from "./terminalStatusView";

const worker = { name: "mac-vm", pmVersion: "0.10.3" };

describe("terminalStatusView", () => {
  it("shows nothing for a live terminal", () => {
    expect(terminalStatusView({ phase: "online" }, { connecting: false, worker })).toBeNull();
  });

  it("asks only for a wait while a connection is being made", () => {
    const status = { phase: "reconnecting", canRetry: false } as const;
    expect(terminalStatusView(status, { connecting: true, worker })).toEqual({
      level: "wait", title: "Connecting…", actions: [],
    });
    expect(terminalStatusView(status, { connecting: false, worker })?.title).toBe("Reconnecting…");
  });

  it("says a lost connection is recovering and offers to retry now", () => {
    const view = terminalStatusView(
      { phase: "reconnecting", canRetry: true, lastError: "connection closed (1006)", problem: "connection" },
      { connecting: false, worker },
    );
    expect(view).toMatchObject({ level: "recovering", detail: "connection closed (1006)" });
    expect(view?.actions).toEqual([{ kind: "retry", label: "Retry now", primary: true }]);
  });

  it("names an offline worker and points at it instead of offering a retry", () => {
    const view = terminalStatusView(
      { phase: "reconnecting", canRetry: false, lastError: "worker 3 cannot be reached at 10.0.0.4:7677", problem: "worker" },
      { connecting: false, worker },
    );
    expect(view).toMatchObject({
      level: "recovering",
      title: "Worker mac-vm is offline",
      detail: "Worker 3 cannot be reached at 10.0.0.4:7677",
    });
    expect(view?.actions.map((action) => action.kind)).toEqual(["view-worker"]);
  });

  it("blocks on a worker with an older terminal engine and names the fix", () => {
    const view = terminalStatusView(
      { phase: "reconnecting", canRetry: true, lastError: "Update this worker.", problem: "incompatible" },
      { connecting: false, worker },
    );
    expect(view).toMatchObject({ level: "blocked", title: "Update worker mac-vm to open this terminal", meta: "worker 0.10.3" });
    expect(view?.actions.map((action) => action.kind)).toEqual(["view-worker", "retry"]);
    expect(terminalStatusView(
      { phase: "reconnecting", canRetry: true, problem: "incompatible" },
      { connecting: false, worker: null },
    )?.title).toBe("Update this terminal's worker to open it");
  });

  it("blocks on an out-of-date page with a reload as the one action", () => {
    const view = terminalStatusView(
      { phase: "reconnecting", canRetry: true, lastError: "Reload this page", problem: "page-outdated" },
      { connecting: false, worker },
    );
    expect(view).toMatchObject({ level: "blocked", title: "Reload to open this terminal" });
    expect(view?.actions).toEqual([{ kind: "reload", label: "Reload page", primary: true }]);
  });

  it("blocks on a renderer that failed to start", () => {
    expect(terminalRendererFailureView("WebAssembly unavailable")).toMatchObject({
      level: "blocked", detail: "WebAssembly unavailable",
    });
  });
});

describe("terminalStatusWorker", () => {
  const state: AppState = {
    ...initialState,
    workers: new Map([["3", create(WorkerSchema, { id: 3n, name: "mac-vm", hostname: "mac-vm.local", pmVersion: "0.10.3" })]]),
    sessions: new Map([["9", create(SessionSchema, { id: 9n, workerId: 3n })]]),
    terminals: new Map([["21", create(TerminalSchema, { id: 21n, sessionId: 9n })]]),
  };

  it("finds the worker through a session or through one of its terminals", () => {
    expect(terminalStatusWorker(state, { sessionId: 9n })).toEqual({ name: "mac-vm", pmVersion: "0.10.3" });
    expect(terminalStatusWorker(state, { terminalId: 21n })).toEqual({ name: "mac-vm", pmVersion: "0.10.3" });
  });

  it("knows no worker for a terminal it has not heard of", () => {
    expect(terminalStatusWorker(state, { terminalId: 99n })).toBeNull();
  });
});

describe("renderTerminalStatus", () => {
  it("renders the level, text and actions and hides the element for no view", () => {
    const el = document.createElement("div");
    const onAction = vi.fn();
    renderTerminalStatus(el, {
      level: "recovering",
      title: "Connection to this terminal lost",
      detail: "connection closed (1006)",
      actions: [{ kind: "retry", label: "Retry now", primary: true }],
    }, onAction);
    expect(el.hidden).toBe(false);
    expect(el.className).toBe("terminal-stream-status is-recovering");
    expect(el.getAttribute("role")).toBe("status");
    expect(el.querySelector(".terminal-status-detail")?.textContent).toBe("connection closed (1006)");
    const button = el.querySelector("button")!;
    expect(button.classList.contains("is-primary")).toBe(true);
    button.click();
    expect(onAction).toHaveBeenCalledWith("retry");

    renderTerminalStatus(el, null, onAction);
    expect(el.hidden).toBe(true);
    expect(el.childElementCount).toBe(0);
  });

  it("marks a blocked state as an alert", () => {
    const el = document.createElement("div");
    renderTerminalStatus(el, terminalRendererFailureView("failed"), () => {});
    expect(el.getAttribute("role")).toBe("alert");
  });
});
