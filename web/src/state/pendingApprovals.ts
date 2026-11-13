import { useEffect, useRef, useState } from "react";
import { newlyPending, pendingCount, type ApprovalSummary } from "@puppet-master/client-core/approvals";
import { coalescedRefetch } from "@puppet-master/client-core/refetch";
import { listApprovals } from "../api/approvals";
import { useConnectionsChanged } from "./hooks";

/** The number of waiting approvals, and a callback for each newly waiting one; a failed fetch keeps the last count. */
export function usePendingApprovals(onFresh?: (approval: ApprovalSummary) => void): number {
  const [count, setCount] = useState(0);
  const seen = useRef<Set<string> | null>(null);
  const fresh = useRef(onFresh);
  fresh.current = onFresh;
  const stopped = useRef(false);
  const [refetch] = useState(() =>
    coalescedRefetch(async () => {
      try {
        const approvals = await listApprovals();
        if (stopped.current) return;
        const next = newlyPending(seen.current, approvals);
        seen.current = next.seen;
        setCount(pendingCount(approvals));
        for (const approval of next.fresh) fresh.current?.(approval);
      } catch {
        // Keep the last count; the next change or reconnect fetches again.
      }
    }),
  );
  useEffect(() => {
    stopped.current = false;
    void refetch();
    return () => {
      stopped.current = true;
    };
  }, [refetch]);
  useConnectionsChanged(() => void refetch());
  return count;
}
