import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import { afterEach, describe, expect, it, vi } from "vitest";

import {
  ClientMessageSchema,
  ServerMessageSchema,
  SessionSchema,
  SessionState,
  SnapshotSchema,
} from "../gen/pm/v1/pm_pb";
import { SOCKET_OPEN, type SocketLike, type SocketMessageEvent } from "../platform";
import { PmClient } from "./client";

const SOCKET_CLOSED = 3;

class FakeSocket implements SocketLike {
  binaryType = "arraybuffer";
  readyState = 0;
  onopen: (() => void) | null = null;
  onmessage: ((event: SocketMessageEvent) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: ((event: { code: number; reason: string }) => void) | null = null;
  readonly sent: string[] = [];
  closeCalls = 0;

  send(data: ArrayBufferLike | Uint8Array): void {
    this.sent.push(fromBinary(ClientMessageSchema, data as Uint8Array).msg.case ?? "");
  }

  close(): void {
    this.closeCalls += 1;
    this.readyState = SOCKET_CLOSED;
  }

  open(): void {
    this.readyState = SOCKET_OPEN;
    this.onopen?.();
  }

  /**
   * A suspended app's connection is torn down by the OS without the frozen
   * JS runtime ever receiving a close event, so readyState still reads open.
   */
  killSilently(): void {
    this.onmessage = null;
  }

  deliverSnapshot(ids: bigint[]): void {
    const snapshot = create(SnapshotSchema, {
      sessions: ids.map((id) =>
        create(SessionSchema, { id, taskTitle: `task ${id}`, state: SessionState.WORKING }),
      ),
    });
    const bytes = toBinary(
      ServerMessageSchema,
      create(ServerMessageSchema, { msg: { case: "snapshot", value: snapshot } }),
    );
    this.onmessage?.({
      data: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength),
    });
  }
}

function harness() {
  const sockets: FakeSocket[] = [];
  const client = new PmClient(() => {
    const socket = new FakeSocket();
    sockets.push(socket);
    return socket;
  });
  return { client, sockets, latest: () => sockets[sockets.length - 1] };
}

function sessionIds(client: PmClient): string[] {
  return [...client.getState().sessions.keys()].sort();
}

afterEach(() => {
  vi.useRealTimers();
});

describe("PmClient.start", () => {
  it("reconnects and resubscribes after a stop", () => {
    const { client, sockets, latest } = harness();
    client.start();
    latest().open();
    latest().deliverSnapshot([1n, 2n]);
    client.stop();

    client.start();
    expect(sockets).toHaveLength(2);
    latest().open();
    expect(latest().sent).toEqual(["subscribe"]);
    latest().deliverSnapshot([1n, 3n]);
    expect(sessionIds(client)).toEqual(["1", "3"]);
  });

  it("reconnects when the client was never stopped and its socket died silently", () => {
    const { client, sockets, latest } = harness();
    client.start();
    latest().open();
    latest().deliverSnapshot([1n, 2n]);
    const dead = latest();
    dead.killSilently();

    // The app resumes and starts the client again without a stop in between,
    // which is what happens when a refresh restarted it while backgrounded.
    client.start();
    expect(sockets).toHaveLength(2);
    expect(dead.closeCalls).toBe(1);
    latest().open();
    expect(latest().sent).toEqual(["subscribe"]);
    latest().deliverSnapshot([2n, 3n]);
    expect(sessionIds(client)).toEqual(["2", "3"]);
  });

  it("drops sessions the fresh snapshot no longer lists", () => {
    const { client, latest } = harness();
    client.start();
    latest().open();
    latest().deliverSnapshot([1n, 2n]);
    client.start();
    latest().open();
    latest().deliverSnapshot([1n]);
    expect(sessionIds(client)).toEqual(["1"]);
  });

  it("ignores a late close from the socket it replaced", () => {
    const { client, sockets, latest } = harness();
    client.start();
    latest().open();
    const replaced = latest();
    client.start();
    latest().open();

    replaced.onclose?.({ code: 1001, reason: "" });

    expect(sockets).toHaveLength(2);
    expect(client.getState().conn).toBe("online");
  });
});

describe("PmClient.refresh", () => {
  it("resolves once the reconnect's snapshot has been applied", async () => {
    const { client, latest } = harness();
    client.start();
    latest().open();
    latest().deliverSnapshot([1n]);

    const pending = client.refresh();
    latest().open();
    latest().deliverSnapshot([1n, 2n]);

    await expect(pending).resolves.toBeUndefined();
    expect(sessionIds(client)).toEqual(["1", "2"]);
  });

  it("rejects when no snapshot arrives before the timeout", async () => {
    vi.useFakeTimers();
    const { client } = harness();
    const pending = client.refresh(5_000);
    const settled = expect(pending).rejects.toThrow("refresh timed out");
    await vi.advanceTimersByTimeAsync(5_000);
    await settled;
  });

  it("rejects when the client stops while the refresh is outstanding", async () => {
    const { client } = harness();
    const pending = client.refresh();
    const settled = expect(pending).rejects.toThrow("client stopped");
    client.stop();
    await settled;
  });

  it("does not settle a later refresh with an earlier one's snapshot", async () => {
    const { client, latest } = harness();
    const first = client.refresh();
    latest().open();
    latest().deliverSnapshot([1n]);
    await expect(first).resolves.toBeUndefined();

    const second = client.refresh();
    let settled = false;
    void second.then(() => { settled = true; }, () => { settled = true; });
    await Promise.resolve();
    expect(settled).toBe(false);

    latest().open();
    latest().deliverSnapshot([1n, 2n]);
    await expect(second).resolves.toBeUndefined();
  });
});
