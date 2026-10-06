import { useEffect, useRef, useState } from "react";
import { newlyPending, pendingCount, type ApprovalSummary } from "@puppet-master/client-core/approvals";
import { listApprovals } from "../api/approvals";

const POLL_MS = 15_000;

/** The number of waiting approvals, and a callback for each newly waiting one; a failed poll keeps the last count. */
export function usePendingApprovals(onFresh?: (approval: ApprovalSummary) => void): number {
  const [count, setCount] = useState(0);
  const seen = useRef<Set<string> | null>(null);
  const fresh = useRef(onFresh);
  fresh.current = onFresh;
  useEffect(() => {
    let stopped = false;
    const poll = async () => {
      try {
        const approvals = await listApprovals();
        if (stopped) return;
        const next = newlyPending(seen.current, approvals);
        seen.current = next.seen;
        setCount(pendingCount(approvals));
        for (const approval of next.fresh) fresh.current?.(approval);
      } catch {
        // Keep the last count; see above.
      }
    };
    void poll();
    const timer = setInterval(() => void poll(), POLL_MS);
    return () => {
      stopped = true;
      clearInterval(timer);
    };
  }, []);
  return count;
}
