import type { DeviceAuthSession } from "../auth/session";

const HTTP_UNAUTHORIZED = 401;

export type PlanDecisionMode = "single" | "multiple" | "dialogue";
export type PlanDecisionState = "open" | "waiting" | "resolved";

export interface PlanOption {
  id: number;
  key: string;
  label: string;
  detailMarkdown: string;
  recommended?: boolean;
}

export interface PlanResponse {
  selectedOptionKeys: string[];
  customLabel: string;
  customDetailMarkdown: string;
  notes: Record<string, string>;
  submittedAtUnixMs: number;
}

export interface PlanDecisionDraftState {
  selectedOptionKeys: string[];
  customLabel: string;
  customDetailMarkdown: string;
  notes: Record<string, string>;
  updatedAtUnixMs: number;
}

export interface PlanDecision {
  id: number;
  key: string;
  title: string;
  promptMarkdown: string;
  detailMarkdown: string;
  mode: PlanDecisionMode;
  state: PlanDecisionState;
  allowCustom: boolean;
  requireSelection: boolean;
  batchKey: string | null;
  batchPosition: number | null;
  resolutionMarkdown: string;
  options: PlanOption[];
  response: PlanResponse | null;
  draft: PlanDecisionDraftState | null;
  createdAtUnixMs: number;
  updatedAtUnixMs: number;
}

export interface PlanMessage {
  id: number;
  decisionId: number | null;
  author: "user" | "session";
  sessionId: number | null;
  body: string;
  createdAtUnixMs: number;
}

export interface PlanDetail {
  plan: {
    id: number;
    name: string;
    summary: string;
    state: "active" | "accepted" | "archived";
    revision: number;
    activeDecisionId: number | null;
    activeDecisionIds: number[];
    markdownPath: string;
  };
  markdown: string;
  decisions: PlanDecision[];
  messages: PlanMessage[];
}

export interface PlanResponseInput {
  selectedOptionKeys: string[];
  customLabel: string;
  customDetailMarkdown: string;
  notes: Record<string, string>;
}

async function planFetch<T>(
  baseUrl: string,
  accessToken: string,
  path: string,
  init?: { method?: string; body?: string; signal?: AbortSignal },
): Promise<T> {
  const url = `${baseUrl.replace(/\/+$/, "")}${path}`;
  const response = await fetch(url, {
    method: init?.method ?? "GET",
    headers: {
      "Content-Type": "application/json",
      Authorization: `Bearer ${accessToken}`,
    },
    body: init?.body,
    signal: init?.signal,
  });
  if (response.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  if (!response.ok) {
    const body = (await response.json().catch(() => null)) as { error?: string } | null;
    throw new Error(body?.error || `plan request failed (${response.status})`);
  }
  return (await response.json()) as T;
}

export function fetchPlan(
  auth: DeviceAuthSession,
  baseUrl: string,
  planId: string,
  signal?: AbortSignal,
): Promise<PlanDetail> {
  return auth.withAccessToken(baseUrl, (token) =>
    planFetch<PlanDetail>(baseUrl, token, `/api/plans/${planId}`, { signal }),
  );
}

export function submitPlanDecision(
  auth: DeviceAuthSession,
  baseUrl: string,
  planId: string,
  decisionId: number,
  response: PlanResponseInput,
): Promise<unknown> {
  return auth.withAccessToken(baseUrl, (token) =>
    planFetch(baseUrl, token, `/api/plans/${planId}/decisions/${decisionId}/respond`, {
      method: "POST",
      body: JSON.stringify(response),
    }),
  );
}

export function submitPlanDecisions(
  auth: DeviceAuthSession,
  baseUrl: string,
  planId: string,
  responses: Array<{ decisionId: number; response: PlanResponseInput }>,
): Promise<{ deliveryState: "queued" }> {
  return auth.withAccessToken(baseUrl, (token) =>
    planFetch<{ deliveryState: "queued" }>(baseUrl, token, `/api/plans/${planId}/decisions/respond`, {
      method: "POST",
      body: JSON.stringify({ responses }),
    }),
  );
}

export function savePlanDraft(
  auth: DeviceAuthSession,
  baseUrl: string,
  planId: string,
  decisionId: number,
  draft: PlanResponseInput,
): Promise<{ status: "saved" }> {
  return auth.withAccessToken(baseUrl, (token) =>
    planFetch<{ status: "saved" }>(baseUrl, token, `/api/plans/${planId}/decisions/${decisionId}/draft`, {
      method: "PUT",
      body: JSON.stringify(draft),
    }),
  );
}

export function savePlanDrafts(
  auth: DeviceAuthSession,
  baseUrl: string,
  planId: string,
  drafts: Array<{ decisionId: number; draft: PlanResponseInput }>,
): Promise<{ status: "saved" }> {
  return auth.withAccessToken(baseUrl, (token) =>
    planFetch<{ status: "saved" }>(baseUrl, token, `/api/plans/${planId}/decisions/drafts`, {
      method: "PUT",
      body: JSON.stringify({ drafts }),
    }),
  );
}

export function postPlanMessage(
  auth: DeviceAuthSession,
  baseUrl: string,
  planId: string,
  decisionId: number | null,
  body: string,
): Promise<PlanMessage> {
  return auth.withAccessToken(baseUrl, (token) =>
    planFetch<PlanMessage>(baseUrl, token, `/api/plans/${planId}/messages`, {
      method: "POST",
      body: JSON.stringify({ decisionId, body }),
    }),
  );
}
