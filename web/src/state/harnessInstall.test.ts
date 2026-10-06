import { afterEach, describe, expect, it, vi } from "vitest";
import { AgentKind } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { harnessStatus, type HarnessReply } from "../api/harness";
import { prepareHarness, InstallCancelled } from "./harnessInstall";
import type { SpawnRequest } from "./spawnSubmit";

vi.mock("../api/harness", () => ({ harnessStatus: vi.fn() }));
afterEach(() => { vi.resetAllMocks(); vi.useRealTimers(); });
const request = { projectId: "1", workerId: 2n } as SpawnRequest;
const reply = (state: HarnessReply["status"]["state"], error = ""): HarnessReply => ({
  agent: "claude", status: { state, command: "installer", output: "download output", error },
});

describe("harness installation before spawn", () => {
  it("uses the resolved agent and never prompts or installs an existing harness", async () => {
    vi.mocked(harnessStatus).mockResolvedValue(reply("ready"));
    const confirm = vi.fn();
    const result = await prepareHarness(request, new AbortController().signal, vi.fn(), confirm);
    expect(result.agent).toBe(AgentKind.CLAUDE_CODE);
    expect(confirm).not.toHaveBeenCalled();
    expect(harnessStatus).toHaveBeenCalledTimes(1);
    expect(vi.mocked(harnessStatus).mock.calls[0][1]).toBe(false);
  });

  it("cancel prevents installation and launch", async () => {
    vi.mocked(harnessStatus).mockResolvedValue(reply("missing"));
    await expect(prepareHarness(request, new AbortController().signal, vi.fn(), async () => false)).rejects.toBeInstanceOf(InstallCancelled);
    expect(harnessStatus).toHaveBeenCalledTimes(1);
  });

  it("waits for confirmation, reports progress, then allows launch", async () => {
    vi.useFakeTimers();
    vi.mocked(harnessStatus).mockResolvedValueOnce(reply("missing"))
      .mockResolvedValueOnce(reply("installing"))
      .mockResolvedValueOnce(reply("ready"));
    let accept!: (value: boolean) => void;
    const update = vi.fn();
    const result = prepareHarness(request, new AbortController().signal, update, () => new Promise((resolve) => { accept = resolve; }));
    await vi.advanceTimersByTimeAsync(0);
    expect(harnessStatus).toHaveBeenCalledTimes(1);
    accept(true);
    await vi.advanceTimersByTimeAsync(0);
    expect(update).toHaveBeenLastCalledWith(reply("installing"));
    expect(vi.mocked(harnessStatus).mock.calls[1][1]).toBe(true);
    await vi.runAllTimersAsync();
    await expect(result).resolves.toMatchObject({ agent: AgentKind.CLAUDE_CODE });
    expect(vi.mocked(harnessStatus).mock.calls[2][1]).toBe(false);
  });

  it("keeps failure details and output and refuses to launch", async () => {
    vi.mocked(harnessStatus).mockResolvedValueOnce(reply("missing"))
      .mockResolvedValueOnce(reply("failed", "exit status 7"));
    const update = vi.fn();
    await expect(prepareHarness(request, new AbortController().signal, update, async () => true)).rejects.toThrow("exit status 7");
    expect(update).toHaveBeenLastCalledWith(reply("failed", "exit status 7"));
  });

  it("a disconnected worker stops polling and reports the error", async () => {
    vi.mocked(harnessStatus).mockResolvedValueOnce(reply("missing"))
      .mockRejectedValueOnce(new Error("worker is offline"));
    await expect(prepareHarness(request, new AbortController().signal, vi.fn(), async () => true)).rejects.toThrow("worker is offline");
  });

  it("older workers retain their existing spawn flow", async () => {
    vi.mocked(harnessStatus).mockResolvedValue(reply("unsupported"));
    const confirm = vi.fn();
    await expect(prepareHarness(request, new AbortController().signal, vi.fn(), confirm)).resolves.toBeDefined();
    expect(confirm).not.toHaveBeenCalled();
  });

  it("closing the form before confirming does not install", async () => {
    vi.mocked(harnessStatus).mockResolvedValue(reply("missing"));
    const controller = new AbortController();
    await expect(prepareHarness(request, controller.signal, vi.fn(), async () => {
      controller.abort(); return true;
    })).rejects.toThrow();
    expect(harnessStatus).toHaveBeenCalledTimes(1);
  });
});
