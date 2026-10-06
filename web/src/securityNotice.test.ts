import { describe, expect, it } from "vitest";
import { create } from "@bufbuild/protobuf";
import { SecurityNoticeKind, SecurityNoticeSchema } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { securityNoticeTitle } from "./securityNotice";

const notice = (kind: SecurityNoticeKind) =>
  create(SecurityNoticeSchema, { kind, subject: "build-box", detail: "d" });

describe("securityNoticeTitle", () => {
  /// Each kind says what was granted, because the user is being asked to
  /// recognise their own action or fail to recognise it.
  it("names what happened for every kind", () => {
    expect(securityNoticeTitle(notice(SecurityNoticeKind.DEVICE_ENROLLED))).toBe(
      "A device was enrolled",
    );
    expect(securityNoticeTitle(notice(SecurityNoticeKind.HOST_ENROLLED))).toBe("A worker joined");
    expect(securityNoticeTitle(notice(SecurityNoticeKind.HOST_KEY_REPLACED))).toBe(
      "A worker was replaced",
    );
    expect(securityNoticeTitle(notice(SecurityNoticeKind.INSTRUCTIONS_REWRITTEN))).toBe(
      "Standing instructions were rewritten",
    );
  });

  /// A kind this build does not know still gets said rather than dropped: a
  /// newer daemon adding one must not make its notices silent.
  it("still says something for a kind it does not know", () => {
    expect(securityNoticeTitle(notice(99 as SecurityNoticeKind))).toBe("Security notice");
  });
});
