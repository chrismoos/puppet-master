import { beforeEach, afterEach, describe, expect, it, vi } from "vitest";
import { createWorkspace, deleteWorkspace, listWorkspaces, reorderWorkspaces, updateWorkspace, type SavedWorkspace } from "./workspaces";
import { seedAccessToken } from "./token.fixture";

// Every authenticated call mints a token when it holds none, which would
// otherwise be the first call a stub answers.
beforeEach(seedAccessToken);

const layout = { kind: "pane" as const, paneId: "pane-one", terminalId: "7" };
const workspace: SavedWorkspace = { id: 3, name: "release", layout, createdAtUnixMs: 1, updatedAtUnixMs: 2, position: 0 };

function response(body: unknown, status = 200): Response {
  return new Response(body === null ? null : JSON.stringify(body), {
    status,
    headers: { "Content-Type": "application/json" },
  });
}

afterEach(() => vi.unstubAllGlobals());

describe("workspace API", () => {
  it("lists daemon-persisted workspaces", async () => {
    vi.stubGlobal("fetch", vi.fn().mockResolvedValue(response({ workspaces: [workspace] })));
    await expect(listWorkspaces()).resolves.toEqual({ workspaces: [workspace] });
  });

  it("persists workspace tab order", async () => {
    const fetchMock = vi.fn().mockResolvedValue(response(null, 204));
    vi.stubGlobal("fetch", fetchMock);
    await reorderWorkspaces([7, 3]);
    expect(fetchMock.mock.calls[0][0]).toBe("/api/workspaces/order");
    expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({ workspace_ids: [7, 3] });
  });

  it("creates and updates recursive layouts", async () => {
    const fetchMock = vi.fn()
      .mockResolvedValueOnce(response(workspace, 201))
      .mockResolvedValueOnce(response(workspace));
    vi.stubGlobal("fetch", fetchMock);
    await createWorkspace("release", layout);
    await updateWorkspace(workspace);
    expect(JSON.parse(fetchMock.mock.calls[0][1].body)).toEqual({ name: "release", layout });
    expect(fetchMock.mock.calls[1][0]).toBe("/api/workspaces/3");
    expect(fetchMock.mock.calls[1][1].method).toBe("PUT");
  });

  it("deletes a workspace and reports daemon errors", async () => {
    const fetchMock = vi.fn()
      .mockResolvedValueOnce(response(null, 204))
      .mockResolvedValueOnce(response({ error: "workspace missing" }, 400));
    vi.stubGlobal("fetch", fetchMock);
    await deleteWorkspace(3);
    expect(fetchMock.mock.calls[0][1].method).toBe("DELETE");
    await expect(deleteWorkspace(4)).rejects.toThrow("workspace missing");
  });
});
