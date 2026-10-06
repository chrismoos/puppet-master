import { authedFetch } from "./token";

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

async function planRequest<T>(path: string, init?: RequestInit): Promise<T> {
  const response = await authedFetch(path, {
    ...init,
    headers: { "content-type": "application/json", ...init?.headers },
  });
  if (response.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  if (!response.ok) {
    const body = await response.json().catch(() => null) as { error?: string } | null;
    throw new Error(body?.error || `plan request failed (${response.status})`);
  }
  return await response.json() as T;
}

export function fetchPlan(id: string, signal?: AbortSignal): Promise<PlanDetail> {
  return planRequest(`/api/plans/${id}`, { signal });
}

export interface PlanResponseInput {
  selectedOptionKeys: string[];
  customLabel: string;
  customDetailMarkdown: string;
  notes: Record<string, string>;
}

export function submitPlanDecision(
  planId: string,
  decisionId: number,
  response: PlanResponseInput,
): Promise<unknown> {
  return planRequest(`/api/plans/${planId}/decisions/${decisionId}/respond`, {
    method: "POST",
    body: JSON.stringify(response),
  });
}

export function submitPlanDecisions(
  planId: string,
  responses: Array<{ decisionId: number; response: PlanResponseInput }>,
): Promise<{ deliveryState: "queued" }> {
  return planRequest(`/api/plans/${planId}/decisions/respond`, {
    method: "POST",
    body: JSON.stringify({ responses }),
  });
}

export function savePlanDraft(
  planId: string,
  decisionId: number,
  draft: PlanResponseInput,
): Promise<{ status: "saved" }> {
  return planRequest(`/api/plans/${planId}/decisions/${decisionId}/draft`, {
    method: "PUT",
    body: JSON.stringify(draft),
  });
}

export function savePlanDrafts(
  planId: string,
  drafts: Array<{ decisionId: number; draft: PlanResponseInput }>,
): Promise<{ status: "saved" }> {
  return planRequest(`/api/plans/${planId}/decisions/drafts`, {
    method: "PUT",
    body: JSON.stringify({ drafts }),
  });
}

export function postPlanMessage(
  planId: string,
  decisionId: number | null,
  body: string,
): Promise<PlanMessage> {
  return planRequest(`/api/plans/${planId}/messages`, {
    method: "POST",
    body: JSON.stringify({ decisionId, body }),
  });
}
