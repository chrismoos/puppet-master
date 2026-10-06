import { describe, expect, it, vi } from "vitest";
import type { SocketCloseEvent, SocketLike, SocketMessageEvent } from "@puppet-master/client-core/platform";
import { deferredSocket, WS_CLOSE_LOCAL_FAILURE } from "./deferredSocket";

const CONNECTING = 0;
const OPEN = 1;
const CLOSED = 3;

function fakeSocket(): SocketLike & { sent: unknown[]; closed: boolean } {
  return {
    binaryType: "blob",
    readyState: OPEN,
    sent: [] as unknown[],
    closed: false,
    send(data: ArrayBufferLike | Uint8Array) {
      this.sent.push(data);
    },
    close() {
      this.closed = true;
    },
    onopen: null as (() => void) | null,
    onmessage: null as ((event: SocketMessageEvent) => void) | null,
    onerror: null as (() => void) | null,
    onclose: null as ((event: SocketCloseEvent) => void) | null,
  } as SocketLike & { sent: unknown[]; closed: boolean };
}

describe("deferredSocket", () => {
  /// The shared client logic asks for a socket and assigns its handlers on the
  /// next line, so a connector that has to mint a ticket first cannot return a
  /// promise. Handlers assigned before the real socket exists still receive
  /// everything it does.
  it("forwards events to handlers assigned before the real socket opened", async () => {
    const real = fakeSocket();
    const facade = deferredSocket(async () => real);
    const events: string[] = [];
    facade.onopen = () => events.push("open");
    facade.onmessage = (event) => events.push(`message:${String(event.data)}`);
    facade.onclose = (event) => events.push(`close:${event.code}`);

    expect(facade.readyState).toBe(CONNECTING);
    await vi.waitFor(() => expect(real.onopen).not.toBeNull());

    real.onopen?.();
    real.onmessage?.({ data: "hello" });
    real.onclose?.({ code: 1000, reason: "done" });

    expect(events).toEqual(["open", "message:hello", "close:1000"]);
  });

  it("carries the binary type set before the connection existed", async () => {
    const real = fakeSocket();
    const facade = deferredSocket(async () => real);
    facade.binaryType = "arraybuffer";

    await vi.waitFor(() => expect(real.binaryType).toBe("arraybuffer"));
  });

  it("closes the real socket when it was closed before opening", async () => {
    const real = fakeSocket();
    const facade = deferredSocket(async () => real);
    facade.close();
    expect(facade.readyState).toBe(CLOSED);

    await vi.waitFor(() => expect(real.closed).toBe(true));
    expect(real.onopen).toBeNull();
  });

  it("reports a failure to open as a close, so the client is not left waiting", async () => {
    const facade = deferredSocket(async () => {
      throw new Error("no ticket");
    });
    const closes: SocketCloseEvent[] = [];
    let errored = false;
    facade.onerror = () => {
      errored = true;
    };
    facade.onclose = (event) => closes.push(event);

    await vi.waitFor(() => expect(closes).toHaveLength(1));
    expect(errored).toBe(true);
    expect(closes[0].code).toBe(WS_CLOSE_LOCAL_FAILURE);
    expect(closes[0].reason).toBe("no ticket");
  });

  /// A ticket the signed-out cannot mint has to read as the daemon refusing the
  /// socket, or the client reconnects against a session that is gone.
  it("reports the close code the caller maps the failure to", async () => {
    const facade = deferredSocket(
      async () => {
        throw new Error("signed out");
      },
      () => 4401,
    );
    const closes: SocketCloseEvent[] = [];
    facade.onclose = (event) => closes.push(event);

    await vi.waitFor(() => expect(closes).toHaveLength(1));
    expect(closes[0].code).toBe(4401);
  });
});
