import type { ApprovalDetail, ApprovalSummary } from "@puppet-master/client-core/approvals";
import { connectionRequest } from "./connections";
import { authedFetch } from "./token";

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

/** Saves the exact argument bytes a call executes with, under the name the controller gives them. */
export async function downloadCallArguments(id: string): Promise<void> {
  const response = await authedFetch(`/api/connection-calls/${encodeURIComponent(id)}/arguments`);
  if (!response.ok) throw new Error(`Download failed (${response.status})`);
  const name =
    /filename="([^"]+)"/.exec(response.headers.get("content-disposition") ?? "")?.[1] ?? "arguments.json";
  const url = URL.createObjectURL(await response.blob());
  const link = document.createElement("a");
  link.href = url;
  link.download = name;
  link.click();
  URL.revokeObjectURL(url);
}
