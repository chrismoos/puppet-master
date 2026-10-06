import { SecurityNoticeKind, type SecurityNotice } from "@puppet-master/client-core/gen/pm/v1/pm_pb";

/**
 * The headline for a notice, in terms of what was granted rather than which
 * endpoint granted it. The user is being asked to recognise their own action or
 * not recognise it, so the wording has to be about the thing, not the mechanism.
 */
export function securityNoticeTitle(notice: SecurityNotice): string {
  switch (notice.kind) {
    case SecurityNoticeKind.DEVICE_ENROLLED:
      return "A device was enrolled";
    case SecurityNoticeKind.HOST_ENROLLED:
      return "A worker joined";
    case SecurityNoticeKind.HOST_KEY_REPLACED:
      return "A worker was replaced";
    case SecurityNoticeKind.INSTRUCTIONS_REWRITTEN:
      return "Standing instructions were rewritten";
    default:
      return "Security notice";
  }
}
