import { create, toBinary } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { ConnectionsChangedSchema, EventSchema, ServerMessageSchema, SnapshotSchema } from "../gen/pm/v1/pm_pb";
import { SOCKET_OPEN, type SocketLike, type SocketMessageEvent } from "../platform";
import { PmClient } from "./client";

class FakeSocket implements SocketLike {
  binaryType = "arraybuffer";
  readyState = SOCKET_OPEN;
  onopen: (() => void) | null = null;
  onmessage: ((event: SocketMessageEvent) => void) | null = null;
  onerror: (() => void) | null = null;
  onclose: (() => void) | null = null;
  send(): void {}
  close(): void {}

  deliver(bytes: Uint8Array): void {
    this.onmessage?.({ data: bytes.buffer.slice(bytes.byteOffset, bytes.byteOffset + bytes.byteLength) });
  }
}

const snapshot = toBinary(
  ServerMessageSchema,
  create(ServerMessageSchema, { msg: { case: "snapshot", value: create(SnapshotSchema, {}) } }),
);

const connectionsChanged = toBinary(
  ServerMessageSchema,
  create(ServerMessageSchema, {
    msg: {
      case: "event",
      value: create(EventSchema, {
        event: { case: "connectionsChanged", value: create(ConnectionsChangedSchema, {}) },
      }),
    },
  }),
);

function connected(): { client: PmClient; socket: FakeSocket } {
  const socket = new FakeSocket();
  const client = new PmClient(() => socket);
  client.start();
  socket.onopen?.();
  return { client, socket };
}

describe("connections change listeners", () => {
  it("fire when the daemon says connections changed", () => {
    const { client, socket } = connected();
    socket.deliver(snapshot);
    let calls = 0;
    client.onConnectionsChanged(() => calls++);
    socket.deliver(connectionsChanged);
    expect(calls).toBe(1);
  });

  it("fire after a snapshot, which follows every reconnect", () => {
    const { client, socket } = connected();
    let calls = 0;
    client.onConnectionsChanged(() => calls++);
    socket.deliver(snapshot);
    socket.deliver(snapshot);
    expect(calls).toBe(2);
  });

  it("stop after unsubscribing", () => {
    const { client, socket } = connected();
    socket.deliver(snapshot);
    let calls = 0;
    const unsubscribe = client.onConnectionsChanged(() => calls++);
    unsubscribe();
    socket.deliver(connectionsChanged);
    expect(calls).toBe(0);
  });
});
