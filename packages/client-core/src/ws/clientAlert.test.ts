import { create, toBinary } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import {
  AgentKind,
  EventSchema,
  ServerMessageSchema,
  SessionAlertKind,
  SessionSchema,
  SessionState,
  SnapshotSchema,
} from "../gen/pm/v1/pm_pb";
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

function session(id: bigint, state: SessionState, unseen = false) {
  return create(SessionSchema, {
    id,
    projectId: 10n,
    agent: AgentKind.CLAUDE_CODE,
    state,
    taskTitle: `task ${id}`,
    createdAtUnixMs: 1_000n,
    needsInputUnseen: unseen && state === SessionState.NEEDS_INPUT,
    idleUnseen: unseen && state === SessionState.IDLE,
  });
}

function connected(): { client: PmClient; socket: FakeSocket } {
  const socket = new FakeSocket();
  const client = new PmClient(() => socket);
  client.start();
  socket.onopen?.();
  return { client, socket };
}

function snapshotOf(...sessions: ReturnType<typeof session>[]): Uint8Array {
  return toBinary(
    ServerMessageSchema,
    create(ServerMessageSchema, {
      msg: { case: "snapshot", value: create(SnapshotSchema, { sessions }) },
    }),
  );
}

function changeOf(changed: ReturnType<typeof session>): Uint8Array {
  return toBinary(
    ServerMessageSchema,
    create(ServerMessageSchema, {
      msg: {
        case: "event",
        value: create(EventSchema, { event: { case: "sessionChanged", value: changed } }),
      },
    }),
  );
}

function alertOf(sessionId: bigint, kind: SessionAlertKind): Uint8Array {
  return toBinary(
    ServerMessageSchema,
    create(ServerMessageSchema, {
      msg: {
        case: "event",
        value: create(EventSchema, {
          event: { case: "sessionAlert", value: { sessionId, kind } },
        }),
      },
    }),
  );
}

describe("PmClient alerts", () => {
  it("hands a raised alert to its listeners with the session and kind", () => {
    const { client, socket } = connected();
    const seen: [bigint, SessionAlertKind][] = [];
    client.onAlert((s, kind) => seen.push([s.id, kind]));

    socket.deliver(snapshotOf(session(7n, SessionState.NEEDS_INPUT)));
    socket.deliver(alertOf(7n, SessionAlertKind.NEEDS_INPUT));

    expect(seen).toEqual([[7n, SessionAlertKind.NEEDS_INPUT]]);
    client.stop();
  });

  // The daemon decides, so the client does not second-guess a repeat: a
  // session already sitting in needs-input still raises when told to.
  it("raises again for a session already in needs-input", () => {
    const { client, socket } = connected();
    let count = 0;
    client.onAlert(() => (count += 1));

    socket.deliver(snapshotOf(session(7n, SessionState.NEEDS_INPUT)));
    socket.deliver(alertOf(7n, SessionAlertKind.NEEDS_INPUT));
    socket.deliver(alertOf(7n, SessionAlertKind.NEEDS_INPUT));

    expect(count).toBe(2);
    client.stop();
  });

  it("drops an alert for a session it does not know", () => {
    const { client, socket } = connected();
    let count = 0;
    client.onAlert(() => (count += 1));

    socket.deliver(snapshotOf());
    socket.deliver(alertOf(404n, SessionAlertKind.NEEDS_INPUT));

    expect(count).toBe(0);
    client.stop();
  });

  it("stops delivering once the listener unsubscribes", () => {
    const { client, socket } = connected();
    let count = 0;
    const off = client.onAlert(() => (count += 1));

    socket.deliver(snapshotOf(session(7n, SessionState.NEEDS_INPUT)));
    socket.deliver(alertOf(7n, SessionAlertKind.COMPLETED));
    off();
    socket.deliver(alertOf(7n, SessionAlertKind.COMPLETED));

    expect(count).toBe(1);
    client.stop();
  });
});

describe("PmClient catch-up alerts after a reconnect", () => {
  function recorded(client: PmClient): [bigint, SessionAlertKind][] {
    const seen: [bigint, SessionAlertKind][] = [];
    client.onAlert((s, kind) => seen.push([s.id, kind]));
    return seen;
  }

  it("raises nothing for unseen sessions on the first hydration", () => {
    const { client, socket } = connected();
    const seen = recorded(client);

    socket.deliver(snapshotOf(session(7n, SessionState.NEEDS_INPUT, true), session(8n, SessionState.IDLE, true)));

    expect(seen).toEqual([]);
    client.stop();
  });

  it("raises needs-input and finished alerts for transitions made while disconnected", () => {
    const { client, socket } = connected();
    const seen = recorded(client);

    socket.deliver(snapshotOf(session(7n, SessionState.WORKING), session(8n, SessionState.WORKING)));
    socket.deliver(
      snapshotOf(
        session(7n, SessionState.NEEDS_INPUT, true),
        session(8n, SessionState.IDLE, true),
        session(9n, SessionState.IDLE, true),
      ),
    );

    expect(seen).toEqual([
      [7n, SessionAlertKind.NEEDS_INPUT],
      [8n, SessionAlertKind.COMPLETED],
      [9n, SessionAlertKind.COMPLETED],
    ]);
    client.stop();
  });

  it("does not repeat an alert the live path already raised", () => {
    const { client, socket } = connected();
    const seen = recorded(client);

    socket.deliver(snapshotOf(session(8n, SessionState.WORKING)));
    socket.deliver(changeOf(session(8n, SessionState.IDLE, true)));
    socket.deliver(alertOf(8n, SessionAlertKind.COMPLETED));
    socket.deliver(snapshotOf(session(8n, SessionState.IDLE, true)));

    expect(seen).toEqual([[8n, SessionAlertKind.COMPLETED]]);
    client.stop();
  });

  it("does not repeat an alert whose session change was missed", () => {
    const { client, socket } = connected();
    const seen = recorded(client);

    socket.deliver(snapshotOf(session(8n, SessionState.WORKING)));
    socket.deliver(alertOf(8n, SessionAlertKind.COMPLETED));
    socket.deliver(snapshotOf(session(8n, SessionState.IDLE, true)));

    expect(seen).toEqual([[8n, SessionAlertKind.COMPLETED]]);
    client.stop();
  });

  it("raises a later episode once the earlier one was seen", () => {
    const { client, socket } = connected();
    const seen = recorded(client);

    socket.deliver(snapshotOf(session(8n, SessionState.WORKING)));
    socket.deliver(changeOf(session(8n, SessionState.IDLE, true)));
    socket.deliver(alertOf(8n, SessionAlertKind.COMPLETED));
    socket.deliver(changeOf(session(8n, SessionState.WORKING)));
    socket.deliver(snapshotOf(session(8n, SessionState.IDLE, true)));

    expect(seen).toEqual([
      [8n, SessionAlertKind.COMPLETED],
      [8n, SessionAlertKind.COMPLETED],
    ]);
    client.stop();
  });

  it("leaves a session that stayed unseen across the reconnect alone", () => {
    const { client, socket } = connected();
    const seen = recorded(client);

    socket.deliver(snapshotOf(session(7n, SessionState.NEEDS_INPUT, true)));
    socket.deliver(snapshotOf(session(7n, SessionState.NEEDS_INPUT, true)));

    expect(seen).toEqual([]);
    client.stop();
  });
});
