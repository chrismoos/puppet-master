import { useEffect, useState } from "react";
import { pendingCount } from "@puppet-master/client-core/approvals";
import { coalescedRefetch } from "@puppet-master/client-core/refetch";

import type { DeviceAuthSession } from "../auth/session";
import { fetchApprovals } from "./approvals";

/** The number of waiting approvals for the menu badge; a failed fetch keeps the last count. */
export function usePendingApprovalCount(
  auth: DeviceAuthSession | null,
  baseUrl: string | null,
  active: boolean,
  onConnectionsChanged: (listener: () => void) => () => void,
): number {
  const [count, setCount] = useState(0);
  useEffect(() => {
    if (!auth || !baseUrl || !active) return;
    let stopped = false;
    const refetch = coalescedRefetch(() =>
      fetchApprovals(auth, baseUrl)
        .then((approvals) => {
          if (!stopped) setCount(pendingCount(approvals));
        })
        .catch(() => {}),
    );
    void refetch();
    const unsubscribe = onConnectionsChanged(() => void refetch());
    return () => {
      stopped = true;
      unsubscribe();
    };
  }, [auth, baseUrl, active, onConnectionsChanged]);
  return count;
}
