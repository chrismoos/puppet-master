import type { ApprovalDetail, ApprovalSummary } from "@puppet-master/client-core/approvals";
import { connectionRequest } from "./connections";

export const listApprovals = () =>
  connectionRequest<ApprovalSummary[]>("/api/connection-approvals");

export const getApproval = (id: string) =>
  connectionRequest<ApprovalDetail>(`/api/connection-approvals/${encodeURIComponent(id)}`);

/** Uses the shared decision endpoint, so the controller's once-only claim applies. */
export const decideApproval = (id: string, approve: boolean) =>
  connectionRequest<{ status: string }>(
    `/api/connection-calls/${encodeURIComponent(id)}/decision`,
    { approve },
  );
