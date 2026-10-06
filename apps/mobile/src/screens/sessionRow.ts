import { sessionStatusLine } from "@puppet-master/client-core/format";
import type { Session } from "@puppet-master/client-core/gen/pm/v1/pm_pb";

/**
 * The line under a session row's goal: the headline, led by the host while
 * searching so matches from different machines can be told apart.
 */
export function sessionRowSubtitle(session: Session, host: string, showHost: boolean): string {
  return [showHost ? host : "", sessionStatusLine(session)].filter(Boolean).join(" · ");
}
