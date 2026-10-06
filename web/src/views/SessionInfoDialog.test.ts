import { create } from "@bufbuild/protobuf";
import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  AgentKind,
  ModelDialect,
  ModelProfileEndpointSchema,
  ModelProfileSchema,
  ModelProfileSource,
  ProjectSchema,
  SessionSchema,
  WorkerSchema,
  type AgentDialects,
} from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { SessionInfoDialog } from "./SessionInfoDialog";

describe("SessionInfoDialog", () => {
  it("renders an accessible, extensible information surface with copy affordances", () => {
    const session = create(SessionSchema, {
      id: 9007199254740993n,
      projectId: 7n,
      workerId: 4n,
      taskTitle: "Unicode launch — équipe",
      cwd: "/srv/checkouts/équipe/非常に長いフォルダー",
    });
    const project = create(ProjectSchema, {
      id: 7n,
      name: "Puppet Master",
      path: "/controller/projects/puppet-master",
    });
    const worker = create(WorkerSchema, {
      id: 4n,
      name: "build-host-east",
      hostname: "worker-04.example",
      online: false,
    });

    const markup = renderToStaticMarkup(createElement(SessionInfoDialog, {
      session,
      project,
      worker,
      returnFocus: null,
      onClose: () => {},
    }));

    expect(markup).toContain('role="dialog"');
    expect(markup).toContain('aria-modal="true"');
    expect(markup).toContain('aria-label="Close session information"');
    expect(markup).toContain("Unicode launch — équipe");
    expect(markup).toContain("Remote");
    expect(markup).toContain("/srv/checkouts/équipe/非常に長いフォルダー");
    expect(markup).toContain("build-host-east");
    expect(markup).toContain("worker-04.example");
    expect(markup).toContain("9007199254740993");
    expect(markup).toContain("Copy launch folder");
    expect(markup).toContain("recorded when the session starts");
    expect(markup).toContain("not a live filesystem or git-status signal");
  });

  it("makes unavailable project, worker, host, and cwd states deliberate", () => {
    const markup = renderToStaticMarkup(createElement(SessionInfoDialog, {
      session: create(SessionSchema, { id: 9n, projectId: 77n, workerId: 88n }),
      returnFocus: null,
      onClose: () => {},
    }));

    expect(markup).toContain("Not recorded");
    expect(markup).toContain("Project 77 unavailable");
    expect(markup).toContain("project may have been deleted");
    expect(markup).toContain("Worker 88 unavailable");
    expect(markup).toContain("no longer registered");
    expect(markup).toContain("Not reported");
  });
});

describe("SessionInfoDialog model section", () => {
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
  ];

  const render = (
    session: ReturnType<typeof create<typeof SessionSchema>>,
    modelProfile?: ReturnType<typeof create<typeof ModelProfileSchema>>,
  ) =>
    renderToStaticMarkup(
      createElement(SessionInfoDialog, {
        session,
        modelProfile,
        agentDialects,
        returnFocus: null,
        onClose: () => {},
      }),
    );

  it("shows the resolved profile, model, and endpoint with its provenance", () => {
    const html = render(
      create(SessionSchema, {
        id: 3n,
        agent: AgentKind.CLAUDE_CODE,
        modelProfileId: 11n,
        modelProfileSource: ModelProfileSource.BUCKET,
      }),
      create(ModelProfileSchema, {
        id: 11n,
        name: "Gateway",
        keySet: true,
        endpoints: [
          create(ModelProfileEndpointSchema, {
            profileId: 11n,
            dialect: ModelDialect.ANTHROPIC_MESSAGES,
            model: "gw/big",
            baseUrl: "https://gw.example/v1",
            backgroundModel: "gw/small",
          }),
        ],
      }),
    );
    expect(html).toContain("Gateway");
    expect(html).toContain("gw/big");
    expect(html).toContain("https://gw.example/v1");
    expect(html).toContain("anthropic-messages");
    expect(html).toContain("Resolved from the bucket.");
  });

  it("reports a profile that cannot serve the session's agent", () => {
    const html = render(
      create(SessionSchema, { id: 3n, agent: AgentKind.CODEX, modelProfileId: 11n }),
      create(ModelProfileSchema, {
        id: 11n,
        name: "Claude only",
        endpoints: [
          create(ModelProfileEndpointSchema, {
            profileId: 11n,
            dialect: ModelDialect.ANTHROPIC_MESSAGES,
            model: "gw/big",
          }),
        ],
      }),
    );
    expect(html).toContain("None for this agent");
    expect(html).toContain("a resume would be rejected");
  });

  it("says the agent runs on its own account when no profile is attached", () => {
    const html = render(create(SessionSchema, { id: 3n, agent: AgentKind.CLAUDE_CODE }));
    expect(html).toContain("Agent account");
    expect(html).toContain("the agent runs on its own account");
  });
});
