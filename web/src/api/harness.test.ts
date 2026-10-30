import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { AgentKind } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { harnessStatus } from "./harness";
import { seedAccessToken } from "./token.fixture";
import type { SpawnRequest } from "../state/spawnSubmit";

beforeEach(seedAccessToken);
afterEach(() => vi.unstubAllGlobals());

const request = { projectId: "2", workerId: 0n, agent: AgentKind.CLAUDE_CODE } as SpawnRequest;
const signal = new AbortController().signal;

describe("harness check responses", () => {
  it("returns a ready harness and targets the selected local worker", async () => {
    const reply = { agent: "claude", status: { state: "ready", command: "", output: "", error: "" } };
    const fetch = vi.fn().mockResolvedValue(new Response(JSON.stringify(reply)));
    vi.stubGlobal("fetch", fetch);
    expect(await harnessStatus(request, false, signal)).toEqual(reply);
    expect(JSON.parse(fetch.mock.calls[0]![1].body)).toEqual({ project: 2, worker: 0, agent: "claude", install: false });
  });

  it("reports the actual plain-text gateway error", async () => {
    const message = "The forwarded server closed or returned an invalid HTTP response.";
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(message, { status: 502 })));
    await expect(harnessStatus(request, false, signal)).rejects.toThrow(message);
  });

  it("preserves structured installation errors", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(JSON.stringify({ error: "worker unavailable" }), { status: 400 })));
    await expect(harnessStatus(request, true, signal)).rejects.toThrow("worker unavailable");
  });

  it.each(["", "not JSON", "{}", "null"])("rejects a malformed successful response: %s", async (body) => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(new Response(body)));
    await expect(harnessStatus(request, false, signal)).rejects.toThrow("invalid response");
  });
});
