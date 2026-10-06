import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import {
  BucketSchema,
  ItemSchema,
  ProjectSchema,
  SessionSchema,
  SnapshotSchema,
} from "./gen/pm/v1/pm_pb";
import { classifyHref, parsePmLink, resolvePmLink } from "./pmlink";
import { initialState, reduce } from "./state/reducer";

function hydratedState() {
  return reduce(initialState, {
    type: "snapshot",
    snapshot: create(SnapshotSchema, {
      buckets: [
        create(BucketSchema, { id: 1n, name: "one" }),
        create(BucketSchema, { id: 2n, name: "two" }),
      ],
      projects: [
        create(ProjectSchema, {
          id: 10n,
          bucketId: 1n,
          name: "one",
          path: "/one",
        }),
        create(ProjectSchema, {
          id: 20n,
          bucketId: 2n,
          name: "two",
          path: "/two",
        }),
      ],
      sessions: [
        create(SessionSchema, { id: 47n, projectId: 10n }),
        create(SessionSchema, { id: 48n, projectId: 20n }),
      ],
      items: [
        create(ItemSchema, { id: 33n, bucketId: 1n, title: "same bucket" }),
        create(ItemSchema, { id: 34n, bucketId: 2n, title: "other bucket" }),
        create(ItemSchema, {
          id: 900719925474099312345n,
          bucketId: 1n,
          title: "lossless",
        }),
      ],
    }),
  });
}

describe("parsePmLink", () => {
  it("parses entity links", () => {
    expect(parsePmLink("pm:session/37")).toEqual({ kind: "session", id: "37" });
    expect(parsePmLink("pm:item/3/12")).toEqual({ kind: "item", bucketId: "3", id: "12" });
    expect(parsePmLink("pm:item/12")).toEqual({ kind: "legacyItem", legacyId: "12" });
    expect(parsePmLink("pm:project/4")).toEqual({ kind: "project", id: "4" });
    expect(parsePmLink("pm:bucket/2")).toEqual({ kind: "bucket", id: "2" });
  });

  it("parses spawn prefill links", () => {
    expect(parsePmLink("pm:spawn?project=4&prompt=fix%20it")).toEqual({
      kind: "spawn",
      projectId: "4",
      prompt: "fix it",
    });
    expect(parsePmLink("pm:spawn")).toEqual({
      kind: "spawn",
      projectId: undefined,
      prompt: undefined,
    });
  });

  it("rejects non-pm and malformed links", () => {
    expect(parsePmLink("https://example.invalid/x")).toBeNull();
    expect(parsePmLink("pm:session/abc")).toBeNull();
    expect(parsePmLink("pm:drop-tables/1")).toBeNull();
    expect(parsePmLink("pm:spawn?project=abc")).toEqual({
      kind: "spawn",
      projectId: undefined,
      prompt: undefined,
    });
  });
});

describe("classifyHref", () => {
  it("routes pm links internally and http(s) externally", () => {
    expect(classifyHref("pm:item/2/1")).toEqual({
      kind: "pm",
      link: { kind: "item", bucketId: "2", id: "1" },
    });
    expect(classifyHref("https://example.invalid/pr/1")).toEqual({
      kind: "external",
    });
  });

  it("renders unknown schemes inert", () => {
    expect(classifyHref("javascript:alert(1)")).toEqual({ kind: "inert" });
    expect(classifyHref("data:text/html,x")).toEqual({ kind: "inert" });
    expect(classifyHref("file:///etc/passwd")).toEqual({ kind: "inert" });
  });
});

describe("resolvePmLink", () => {
  it("revalidates a qualified item in the source session bucket", () => {
    const state = hydratedState();

    expect(resolvePmLink(state, { kind: "item", bucketId: "1", id: "33" }, "47")).toEqual({
      kind: "item",
      bucketId: "1",
      id: "33",
    });
    expect(resolvePmLink(state, { kind: "item", bucketId: "2", id: "34" }, "47")).toBeNull();
  });

  it("rejects missing targets, deleted source sessions, and unhydrated state", () => {
    const state = hydratedState();

    expect(resolvePmLink(state, { kind: "item", bucketId: "1", id: "999" }, "47")).toBeNull();
    expect(resolvePmLink(state, { kind: "item", bucketId: "1", id: "33" }, "999")).toBeNull();
    expect(
      resolvePmLink(initialState, { kind: "item", bucketId: "1", id: "33" }, "47"),
    ).toBeNull();
  });

  it("keeps ids beyond Number.MAX_SAFE_INTEGER lossless", () => {
    const id = "900719925474099312345";

    expect(resolvePmLink(hydratedState(), { kind: "item", bucketId: "1", id }, "47")).toEqual({
      kind: "item",
      bucketId: "1",
      id,
    });
  });
});
