import { openableForwardUrl } from "@puppet-master/client-core/api/forwards";
import type { SessionForward } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import type { JsonFetch } from "@puppet-master/client-core/platform";
import { forwardForUrl } from "@puppet-master/client-core/state/forwards";

export interface ForwardLinkDeps {
  /** The forwards the controller currently reports. */
  forwards(): Iterable<SessionForward>;
  /** Runs a call against the controller with this device's credential. */
  withFetch<T>(call: (fetch: JsonFetch) => Promise<T>): Promise<T>;
}

/**
 * The URL to hand Safari for a tapped link.
 *
 * Safari holds no dashboard session, only the app does, so a forward link
 * opened bare lands on the dashboard's sign-in page. A link into a known
 * forward is opened with a token scoped to that forward instead, deep path
 * and all, and the server checks the destination before it mints one. Any
 * other link, and a forward link whose token cannot be minted, opens
 * unchanged.
 */
export async function resolveTappedUrl(url: string, deps: ForwardLinkDeps): Promise<string> {
  const forward = forwardForUrl(deps.forwards(), url);
  if (!forward) return url;
  try {
    return await deps.withFetch((fetch) => openableForwardUrl(fetch, forward.id.toString(), url, true));
  } catch {
    return url;
  }
}
