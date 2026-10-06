import { useEffect, useState } from "react";
import { pendingCount } from "@puppet-master/client-core/approvals";

import type { DeviceAuthSession } from "../auth/session";
import { fetchApprovals } from "./approvals";

const POLL_MS = 30_000;

/** The number of waiting approvals for the menu badge; a failed poll keeps the last count. */
export function usePendingApprovalCount(
  auth: DeviceAuthSession | null,
  baseUrl: string | null,
  active: boolean,
): number {
  const [count, setCount] = useState(0);
  useEffect(() => {
    if (!auth || !baseUrl || !active) return;
    let stopped = false;
    const poll = () =>
      fetchApprovals(auth, baseUrl)
        .then((approvals) => {
          if (!stopped) setCount(pendingCount(approvals));
        })
        .catch(() => {});
    void poll();
    const timer = setInterval(() => void poll(), POLL_MS);
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  }, [auth, baseUrl, active]);
  return count;
}
