import {
  type ActivityReport,
  parseReportsResponse,
  sortNewestFirst,
} from "@puppet-master/client-core/api/reports";

export {
  type ReportKind,
  type ActivityReport,
  sortNewestFirst,
  mergeReports,
  parseReportsResponse,
} from "@puppet-master/client-core/api/reports";
import { authedFetch } from "./token";

const HTTP_UNAUTHORIZED = 401;

export async function fetchReports(
  sessionId: string,
  signal?: AbortSignal,
): Promise<ActivityReport[]> {
  const res = await authedFetch(`/api/sessions/${sessionId}/reports`, { signal });
  if (res.status === HTTP_UNAUTHORIZED) throw new Error("not authenticated");
  if (!res.ok) {
    const body = (await res.json().catch(() => null)) as { error?: string } | null;
    throw new Error(body?.error || `report fetch failed (${res.status})`);
  }
  return sortNewestFirst(parseReportsResponse(await res.json()));
}
