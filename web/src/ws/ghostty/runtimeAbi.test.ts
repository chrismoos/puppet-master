import { readFile } from "node:fs/promises";
import { afterAll, beforeAll, describe, expect, it, vi } from "vitest";
import { GhosttyRuntime } from "./runtime";

vi.mock("./vendor/ghostty-vt.wasm?url", () => ({ default: "ghostty-core-wasm" }));
vi.mock("./vendor/ghostty-write-pty.wasm?url&no-inline", () => ({ default: "ghostty-pty-wasm" }));

describe("vendored libghostty-vt runtime", () => {
  let pinnedRevision = "";
  let coreWasm = new Uint8Array();
  let ptyWasm = new Uint8Array();

  beforeAll(async () => {
    pinnedRevision = (await readFile(new URL("./vendor/VERSION", import.meta.url), "utf8")).trim();
    coreWasm = new Uint8Array(await readFile(new URL("./vendor/ghostty-vt.wasm", import.meta.url)));
    ptyWasm = new Uint8Array(await readFile(new URL("./vendor/ghostty-write-pty.wasm", import.meta.url)));
    vi.stubGlobal("fetch", vi.fn(async (url: string) => new Response(
      url === "ghostty-core-wasm" ? coreWasm : ptyWasm,
      { status: 200 },
    )));
  });

  afterAll(() => vi.unstubAllGlobals());

  it("matches the pinned source revision and expected ABI layouts", async () => {
    const runtime = await GhosttyRuntime.load();
    const size = runtime.alloc(8);
    expect(runtime.call("ghostty_build_info", 10, size)).toBe(0);
    const view = runtime.view(size, 8);
    const revision = new TextDecoder().decode(runtime.bytes(view.getUint32(0, true), view.getUint32(4, true)));
    expect(revision).toBe(pinnedRevision);
    expect(runtime.layout("GhosttyTerminalScrollbar").fields.offset?.type).toBe("u64");
    const opaque = runtime.allocOpaque();
    expect(opaque).not.toBe(0);
    runtime.freeOpaque(opaque);
    runtime.free(size, 8);
  });
});
