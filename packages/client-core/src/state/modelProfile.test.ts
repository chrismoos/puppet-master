import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import {
  AgentKind,
  BucketSchema,
  ModelDialect,
  ModelProfileEndpointSchema,
  ModelProfileSchema,
  ProjectSchema,
  type AgentDialects,
} from "../gen/pm/v1/pm_pb";
import {
  backgroundModelApplies,
  coveredAgents,
  profileApplies,
  resolveModelProfile,
  selectEndpoint,
} from "./modelProfile";

const agentDialects: AgentDialects[] = [
  {
    $typeName: "pm.v1.AgentDialects",
    agent: AgentKind.CLAUDE_CODE,
    dialects: [ModelDialect.ANTHROPIC_MESSAGES],
    supportsBackgroundModel: true,
  },
  {
    $typeName: "pm.v1.AgentDialects",
    agent: AgentKind.CODEX,
    dialects: [ModelDialect.OPENAI_RESPONSES],
    supportsBackgroundModel: false,
  },
  {
    $typeName: "pm.v1.AgentDialects",
    agent: AgentKind.ANTIGRAVITY,
    dialects: [],
    supportsBackgroundModel: false,
  },
];

function profile(...dialects: ModelDialect[]) {
  return create(ModelProfileSchema, {
    id: 11n,
    name: "Gateway",
    keySet: true,
    endpoints: dialects.map((dialect) =>
      create(ModelProfileEndpointSchema, { profileId: 11n, dialect, model: `m-${dialect}` }),
    ),
  });
}

describe("resolveModelProfile", () => {
  it("applies explicit, project, then bucket precedence", () => {
    const bucket = create(BucketSchema, { modelProfileId: 1n });
    const project = create(ProjectSchema, { modelProfileId: 2n });
    expect(resolveModelProfile(project, bucket, 3n)).toEqual({ profileId: 3n, source: "explicit" });
    expect(resolveModelProfile(project, bucket)).toEqual({ profileId: 2n, source: "project" });
    expect(resolveModelProfile(create(ProjectSchema), bucket)).toEqual({ profileId: 1n, source: "bucket" });
  });

  it("resolves to nothing when no layer sets a profile", () => {
    expect(resolveModelProfile(create(ProjectSchema), create(BucketSchema))).toBeUndefined();
  });
});

describe("selectEndpoint", () => {
  it("picks the entry matching the agent's dialect", () => {
    const both = profile(ModelDialect.OPENAI_RESPONSES, ModelDialect.ANTHROPIC_MESSAGES);
    expect(selectEndpoint(both, AgentKind.CLAUDE_CODE, agentDialects)?.dialect).toBe(
      ModelDialect.ANTHROPIC_MESSAGES,
    );
    expect(selectEndpoint(both, AgentKind.CODEX, agentDialects)?.dialect).toBe(
      ModelDialect.OPENAI_RESPONSES,
    );
  });

  it("returns nothing when the profile covers no dialect the agent speaks", () => {
    expect(selectEndpoint(profile(ModelDialect.ANTHROPIC_MESSAGES), AgentKind.CODEX, agentDialects)).toBeUndefined();
    expect(selectEndpoint(undefined, AgentKind.CODEX, agentDialects)).toBeUndefined();
  });
});

describe("coveredAgents", () => {
  it("derives coverage from the daemon's capability list", () => {
    expect(coveredAgents(profile(ModelDialect.ANTHROPIC_MESSAGES), agentDialects)).toEqual([
      AgentKind.CLAUDE_CODE,
    ]);
    expect(
      coveredAgents(profile(ModelDialect.ANTHROPIC_MESSAGES, ModelDialect.OPENAI_RESPONSES), agentDialects),
    ).toEqual([AgentKind.CLAUDE_CODE, AgentKind.CODEX]);
  });
});

describe("profileApplies", () => {
  it("is false for an agent that speaks no dialect, so no profile can reach it", () => {
    expect(profileApplies(AgentKind.CLAUDE_CODE, agentDialects)).toBe(true);
    expect(profileApplies(AgentKind.ANTIGRAVITY, agentDialects)).toBe(false);
    expect(coveredAgents(profile(ModelDialect.ANTHROPIC_MESSAGES), agentDialects)).not.toContain(
      AgentKind.ANTIGRAVITY,
    );
  });
});

describe("backgroundModelApplies", () => {
  it("is false when every agent running the dialect ignores the field", () => {
    expect(backgroundModelApplies(ModelDialect.ANTHROPIC_MESSAGES, agentDialects)).toBe(true);
    expect(backgroundModelApplies(ModelDialect.OPENAI_RESPONSES, agentDialects)).toBe(false);
  });

  it("does not claim a field is inert when the daemon reported no agents", () => {
    expect(backgroundModelApplies(ModelDialect.OPENAI_RESPONSES, [])).toBe(true);
  });
});
