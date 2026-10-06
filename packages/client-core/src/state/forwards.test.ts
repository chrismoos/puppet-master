import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import {
  SessionForwardSchema,
  SessionSchema,
  SessionState,
  type Session,
  type SessionForward,
} from "../gen/pm/v1/pm_pb";
import { forwardsForSession, forwardsNewestFirst, visibleForwards } from "./forwards";

function session(id: bigint, overrides: Partial<Omit<Session, "$typeName" | "$unknown">> = {}) {
  return create(SessionSchema, { id, goal: `session ${id} goal`, ...overrides });
}

function forward(id: bigint, sessionId: bigint, label: string, createdAtUnixMs = 0n) {
  return create(SessionForwardSchema, {
    id,
    sessionId,
    label,
    createdAtUnixMs,
    url: `https://pm.example/forwards/${id}/`,
  });
}

function state(sessions: Session[], forwards: SessionForward[]) {
  return {
    sessions: new Map(sessions.map((s) => [s.id.toString(), s])),
    forwards: new Map(forwards.map((f) => [f.id.toString(), f])),
  };
}

const names = (groups: ReturnType<typeof forwardsForSession>) =>
  groups.map((group) => [group.session.id.toString(), group.forwards.map((f) => f.label)]);

describe("forwardsForSession", () => {
  it("lists a session's own forwards in id order and nothing else", () => {
    const groups = forwardsForSession(
      state(
        [session(1n), session(2n)],
        [forward(20n, 1n, "second"), forward(10n, 1n, "first"), forward(30n, 2n, "unrelated")],
      ),
      "1",
    );

    expect(names(groups)).toEqual([["1", ["first", "second"]]]);
  });

  it("puts each spawned worker's forwards in its own group after the session's own", () => {
    const groups = forwardsForSession(
      state(
        [session(1n), session(3n, { spawnedBySessionId: 1n }), session(2n, { spawnedBySessionId: 1n })],
        [forward(11n, 3n, "later worker"), forward(12n, 2n, "earlier worker"), forward(13n, 1n, "own")],
      ),
      "1",
    );

    expect(names(groups)).toEqual([
      ["1", ["own"]],
      ["2", ["earlier worker"]],
      ["3", ["later worker"]],
    ]);
  });

  it("follows spawnedBySessionId transitively, depth first", () => {
    const groups = forwardsForSession(
      state(
        [
          session(1n),
          session(2n, { spawnedBySessionId: 1n }),
          session(3n, { spawnedBySessionId: 2n }),
          session(4n, { spawnedBySessionId: 1n }),
        ],
        [forward(10n, 1n, "own"), forward(11n, 2n, "worker"), forward(12n, 3n, "sub-worker"), forward(13n, 4n, "sibling")],
      ),
      "1",
    );

    expect(names(groups)).toEqual([
      ["1", ["own"]],
      ["2", ["worker"]],
      ["3", ["sub-worker"]],
      ["4", ["sibling"]],
    ]);
  });

  it("omits a descendant with no forwards rather than listing an empty group", () => {
    const groups = forwardsForSession(
      state(
        [session(1n), session(2n, { spawnedBySessionId: 1n }), session(3n, { spawnedBySessionId: 2n })],
        [forward(10n, 3n, "sub-worker")],
      ),
      "1",
    );

    expect(names(groups)).toEqual([["3", ["sub-worker"]]]);
  });

  it("keeps an exited descendant's group while its forward rows exist", () => {
    const groups = forwardsForSession(
      state(
        [session(1n), session(2n, { spawnedBySessionId: 1n, state: SessionState.EXITED, endedAtUnixMs: 500n })],
        [forward(10n, 2n, "still published")],
      ),
      "1",
    );

    expect(names(groups)).toEqual([["2", ["still published"]]]);
  });

  it("visits each session once when spawn links form a cycle", () => {
    const groups = forwardsForSession(
      state(
        [
          session(1n, { spawnedBySessionId: 3n }),
          session(2n, { spawnedBySessionId: 1n }),
          session(3n, { spawnedBySessionId: 2n }),
        ],
        [forward(10n, 1n, "own"), forward(11n, 2n, "worker"), forward(12n, 3n, "sub-worker")],
      ),
      "1",
    );

    expect(names(groups)).toEqual([
      ["1", ["own"]],
      ["2", ["worker"]],
      ["3", ["sub-worker"]],
    ]);
  });

  it("does not loop on a session that reports itself as its own parent", () => {
    const groups = forwardsForSession(
      state([session(1n, { spawnedBySessionId: 1n })], [forward(10n, 1n, "own")]),
      "1",
    );

    expect(names(groups)).toEqual([["1", ["own"]]]);
  });

  it("returns nothing while the session itself is not in state", () => {
    expect(forwardsForSession(state([session(2n, { spawnedBySessionId: 1n })], [forward(10n, 2n, "worker")]), "1"))
      .toEqual([]);
  });

  it("excludes an ancestor's and a cousin's forwards", () => {
    const groups = forwardsForSession(
      state(
        [
          session(1n),
          session(2n, { spawnedBySessionId: 1n }),
          session(3n, { spawnedBySessionId: 1n }),
        ],
        [forward(10n, 1n, "supervisor"), forward(11n, 2n, "own"), forward(12n, 3n, "cousin")],
      ),
      "2",
    );

    expect(names(groups)).toEqual([["2", ["own"]]]);
  });
});

describe("forwardsNewestFirst", () => {
  it("orders every owner's forwards together by publish time, newest first", () => {
    const groups = forwardsForSession(
      state(
        [session(1n), session(2n, { spawnedBySessionId: 1n })],
        [
          forward(10n, 1n, "old own", 100n),
          forward(11n, 2n, "newest worker", 300n),
          forward(12n, 1n, "newer own", 200n),
        ],
      ),
      "1",
    );

    expect(forwardsNewestFirst(groups).map((o) => [o.session.id.toString(), o.forward.label])).toEqual([
      ["2", "newest worker"],
      ["1", "newer own"],
      ["1", "old own"],
    ]);
  });

  it("breaks a publish-time tie by id so a later row still comes first", () => {
    const groups = forwardsForSession(
      state([session(1n)], [forward(10n, 1n, "first", 100n), forward(11n, 1n, "second", 100n)]),
      "1",
    );

    expect(forwardsNewestFirst(groups).map((o) => o.forward.label)).toEqual(["second", "first"]);
  });
});

describe("visibleForwards", () => {
  const list = ["a", "b", "c", "d", "e", "f"];

  it("shows the first few and counts the rest while collapsed", () => {
    expect(visibleForwards(list, false, 4)).toEqual({ shown: ["a", "b", "c", "d"], hidden: 2 });
  });

  it("shows everything once expanded", () => {
    expect(visibleForwards(list, true, 4)).toEqual({ shown: list, hidden: 0 });
  });

  it("does not hide a single entry behind a control that would take its place", () => {
    expect(visibleForwards(list.slice(0, 5), false, 4)).toEqual({ shown: list.slice(0, 5), hidden: 0 });
    expect(visibleForwards(list.slice(0, 4), false, 4)).toEqual({ shown: list.slice(0, 4), hidden: 0 });
  });
});
