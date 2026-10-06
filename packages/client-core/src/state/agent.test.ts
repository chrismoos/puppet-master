import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { AgentKind, BucketSchema, ProjectSchema } from "../gen/pm/v1/pm_pb";
import { AGENTS, agentFromValue, agentLabel, agentValue, resolveAgent } from "./agent";

describe("resolveAgent", () => {
  const bucket = create(BucketSchema, { defaultAgent: AgentKind.CODEX });
  const project = create(ProjectSchema, { defaultAgent: AgentKind.CLAUDE_CODE });

  it("applies explicit, project, bucket, fallback precedence", () => {
    expect(resolveAgent(project, bucket, AgentKind.CODEX)).toEqual({ agent: AgentKind.CODEX, source: "explicit" });
    expect(resolveAgent(project, bucket)).toEqual({ agent: AgentKind.CLAUDE_CODE, source: "project" });
    expect(resolveAgent(create(ProjectSchema), bucket)).toEqual({ agent: AgentKind.CODEX, source: "bucket" });
    expect(resolveAgent(create(ProjectSchema), create(BucketSchema))).toEqual({ agent: AgentKind.CLAUDE_CODE, source: "fallback" });
  });
});

describe("the agent table", () => {
  it("is the single source every agent control reads", () => {
    expect(AGENTS.map((agent) => agent.value)).toEqual([
      "claude",
      "codex",
      "gemini",
      "opencode",
      "antigravity",
    ]);
    for (const agent of AGENTS) {
      expect(agentValue(agent.kind)).toBe(agent.value);
      expect(agentFromValue(agent.value)).toBe(agent.kind);
      expect(agentLabel(agent.kind)).toBe(agent.label);
    }
  });

  it("reports no value for an agent the table does not offer", () => {
    expect(agentValue(undefined)).toBe("");
    expect(agentValue(AgentKind.UNSPECIFIED)).toBe("");
    expect(agentValue(AgentKind.TEST)).toBe("");
    expect(agentFromValue("")).toBeUndefined();
    expect(agentFromValue("test")).toBeUndefined();
  });
});
