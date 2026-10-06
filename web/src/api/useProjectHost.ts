import { useEffect, useState } from "react";

import { fetchProjectHost, type ProjectHostState } from "./projectHost";

/**
 * How the selected project stands on the selected host, refreshed
 * whenever either changes. `null` while nothing is selected or the
 * check has not answered yet.
 *
 * `online` is a dependency rather than a shortcut: a host that
 * reconnects has to be asked again, or the form keeps showing the
 * verdict from while it was down.
 */
export function useProjectHost(
  projectId: bigint | undefined,
  workerId: bigint | undefined,
  online: boolean,
): ProjectHostState | null {
  const [state, setState] = useState<ProjectHostState | null>(null);
  const project = projectId?.toString();
  const worker = workerId?.toString();

  useEffect(() => {
    if (project === undefined || worker === undefined) {
      setState(null);
      return;
    }
    const controller = new AbortController();
    setState(null);
    fetchProjectHost(BigInt(project), BigInt(worker), controller.signal)
      .then(setState)
      // A check that cannot run is not evidence against the project, so
      // the form falls back to what it says about the host itself rather
      // than inventing a reason the spawn would fail.
      .catch(() => undefined);
    return () => controller.abort();
  }, [project, worker, online]);

  return state;
}
