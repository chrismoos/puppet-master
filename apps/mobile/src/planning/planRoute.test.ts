import { describe, expect, it } from "vitest";
import type { Plan } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { PlanState } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { create } from "@bufbuild/protobuf";
import { PlanSchema } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { findActivePlanForSession } from "./planRoute";

describe("findActivePlanForSession", () => {
  it("returns the active plan with a decision", () => {
    const plans = new Map<string, Plan>();
    plans.set("10", create(PlanSchema, {
      id: 10n, owningSessionId: 5n, name: "Architecture Plan",
      state: PlanState.ACTIVE, activeDecisionId: 3n,
    }));

    const target = findActivePlanForSession(5n, plans);
    expect(target).toEqual({ planId: "10", planName: "Architecture Plan" });
  });

  it("returns null when no plan has an active decision", () => {
    const plans = new Map<string, Plan>();
    plans.set("10", create(PlanSchema, {
      id: 10n, owningSessionId: 5n, name: "Architecture Plan",
      state: PlanState.ACTIVE,
    }));

    expect(findActivePlanForSession(5n, plans)).toBeNull();
  });

  it("ignores accepted plans", () => {
    const plans = new Map<string, Plan>();
    plans.set("10", create(PlanSchema, {
      id: 10n, owningSessionId: 5n, name: "Done Plan",
      state: PlanState.ACCEPTED, activeDecisionId: 3n,
    }));

    expect(findActivePlanForSession(5n, plans)).toBeNull();
  });

  it("ignores plans for other sessions", () => {
    const plans = new Map<string, Plan>();
    plans.set("10", create(PlanSchema, {
      id: 10n, owningSessionId: 99n, name: "Other Plan",
      state: PlanState.ACTIVE, activeDecisionId: 1n,
    }));

    expect(findActivePlanForSession(5n, plans)).toBeNull();
  });

  it("returns the first matching plan", () => {
    const plans = new Map<string, Plan>();
    plans.set("10", create(PlanSchema, {
      id: 10n, owningSessionId: 5n, name: "Plan Alpha",
      state: PlanState.ACTIVE, activeDecisionId: 1n,
    }));
    plans.set("11", create(PlanSchema, {
      id: 11n, owningSessionId: 5n, name: "Plan Beta",
      state: PlanState.ACTIVE,
    }));

    expect(findActivePlanForSession(5n, plans)).toEqual({ planId: "10", planName: "Plan Alpha" });
  });
});
