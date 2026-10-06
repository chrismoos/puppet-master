import type { JsonFetch } from "@puppet-master/client-core/platform";
import { authedFetch } from "./token";

/** Daemon HTTP adapter: same-origin fetch carrying the dashboard's access
 * token. The session cookie rides along because the browser attaches it, and
 * the daemon does not read it on these routes. */
export const browserJsonFetch: JsonFetch = (path, options) =>
  authedFetch(path, {
    method: options?.method,
    signal: options?.signal as AbortSignal | undefined,
  });
