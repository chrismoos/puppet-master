import type { ApprovalDetail, ApprovalSummary } from "@puppet-master/client-core/approvals";

import { AuthHttpError } from "../auth/api";
import type { DeviceAuthSession } from "../auth/session";

const HTTP_UNAUTHORIZED = 401;

async function approvalFetch<T>(
  baseUrl: string,
  accessToken: string,
  path: string,
  init?: { method?: string; body?: string; signal?: AbortSignal },
): Promise<T> {
  const response = await fetch(`${baseUrl.replace(/\/+$/, "")}${path}`, {
    method: init?.method ?? "GET",
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${accessToken}`,
    },
    body: init?.body,
    signal: init?.signal,
  });
  // An AuthHttpError lets withAccessToken rotate an expired token and retry.
  if (response.status === HTTP_UNAUTHORIZED) throw new AuthHttpError(HTTP_UNAUTHORIZED, "not authenticated");
  if (!response.ok) {
    const body = (await response.json().catch(() => null)) as { error?: string } | null;
    throw new Error(body?.error || `approval request failed (${response.status})`);
  }
  return (await response.json()) as T;
}

export function fetchApprovals(
  auth: DeviceAuthSession,
  baseUrl: string,
  signal?: AbortSignal,
): Promise<ApprovalSummary[]> {
  return auth.withAccessToken(baseUrl, (token) =>
    approvalFetch<ApprovalSummary[]>(baseUrl, token, "/api/connection-approvals", { signal }),
  );
}

export function fetchApproval(
  auth: DeviceAuthSession,
  baseUrl: string,
  id: string,
  signal?: AbortSignal,
): Promise<ApprovalDetail> {
  return auth.withAccessToken(baseUrl, (token) =>
    approvalFetch<ApprovalDetail>(
      baseUrl,
      token,
      `/api/connection-approvals/${encodeURIComponent(id)}`,
      { signal },
    ),
  );
}

/** Uses the dashboard's decision endpoint, so the controller's once-only claim applies. */
export function decideApproval(
  auth: DeviceAuthSession,
  baseUrl: string,
  id: string,
  approve: boolean,
): Promise<{ status: string }> {
  return auth.withAccessToken(baseUrl, (token) =>
    approvalFetch<{ status: string }>(
      baseUrl,
      token,
      `/api/connection-calls/${encodeURIComponent(id)}/decision`,
      { method: "POST", body: JSON.stringify({ approve }) },
    ),
  );
}
