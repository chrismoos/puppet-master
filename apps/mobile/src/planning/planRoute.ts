import type { Plan } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { PlanState } from "@puppet-master/client-core/gen/pm/v1/pm_pb";

export function findActivePlanForSession(
  sessionId: bigint,
  plans: ReadonlyMap<string, Plan>,
): { planId: string; planName: string } | null {
  for (const [, plan] of plans) {
    if (
      plan.owningSessionId === sessionId &&
      plan.state === PlanState.ACTIVE &&
      plan.activeDecisionId !== undefined
    ) {
      return { planId: plan.id.toString(), planName: plan.name };
    }
  }
  return null;
}
