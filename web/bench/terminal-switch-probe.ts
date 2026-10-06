import { create } from "@bufbuild/protobuf";
import { createElement } from "react";
import { createRoot } from "react-dom/client";
import {
  AgentKind,
  SessionSchema,
  SessionState,
  TerminalKind,
  TerminalRunState,
  TerminalSchema,
} from "../src/gen/pm/v1/pm_pb";
import type { AppState } from "../src/state/reducer";
import { ClientContext } from "../src/state/hooks";
import { SessionPane } from "../src/views/SessionPane";
import type { PmClient, PtyHandle } from "../src/ws/client";
import { TerminalStage } from "../src/ws/terminal";
import { TerminalThemeController } from "../src/theme/controller";
import "@xterm/xterm/css/xterm.css";

const SHELL_COUNT = 24;
const SWITCH_COUNT = 100;
const AGENT_REPLAY_BYTES = 512 * 1024;
const AGENT_REPLAY_LINE = "agent output\r\n";

interface ProbeResult {
  shells: number;
  switches: number;
  agentOpens: number;
  agentCloses: number;
  shellOpens: number;
  shellCloses: number;
  selectionUnmounts: number;
  showP50Ms: number;
  showP99Ms: number;
  showMaxMs: number;
}

const nextFrame = () => new Promise<void>((resolve) => requestAnimationFrame(() => resolve()));
const round = (value: number) => Math.round(value * 100) / 100;

async function selectionUnmountCount(): Promise<number> {
  const session = create(SessionSchema, {
    id: 1n,
    projectId: 1n,
    agent: AgentKind.CODEX,
    state: SessionState.WORKING,
    createdAtUnixMs: BigInt(Date.now()),
    workerId: 0n,
  });
  const agent = create(TerminalSchema, {
    id: 1n,
    sessionId: 1n,
    kind: TerminalKind.AGENT,
    state: TerminalRunState.RUNNING,
    generation: 1n,
  });
  const shell = create(TerminalSchema, {
    id: 2n,
    sessionId: 1n,
    kind: TerminalKind.SHELL,
    state: TerminalRunState.RUNNING,
    generation: 1n,
    title: "Shell",
  });
  const state: AppState = {
    conn: "online",
    hydrated: true,
    buckets: new Map(),
    projects: new Map(),
    sessions: new Map([["1", session]]),
    workers: new Map(),
    terminals: new Map([["1", agent], ["2", shell]]),
    contexts: new Map(),
    forwards: new Map(),
  };
  const client = {
    getState: () => state,
    subscribe: () => () => {},
  } as unknown as PmClient;
  let unmounts = 0;
  const stage = {
    mount: () => {},
    unmount: () => { unmounts += 1; },
    show: () => {},
    showTerminal: () => {},
    terminalCommand: () => undefined,
    onTerminalCommand: () => () => {},
    disposeTerminal: () => {},
  } as unknown as TerminalStage;
  const container = document.createElement("div");
  document.body.appendChild(container);
  const root = createRoot(container);
  root.render(createElement(ClientContext.Provider, { value: client }, createElement(SessionPane, {
    sessionId: "1",
    stage,
    onSelect: () => {},
    onSpawn: () => {},
  })));
  await nextFrame();
  container.querySelector<HTMLButtonElement>(".terminal-tab")?.click();
  await nextFrame();
  const selectionUnmounts = unmounts;
  root.unmount();
  container.remove();
  return selectionUnmounts;
}

async function main(): Promise<void> {
  const selectionUnmounts = await selectionUnmountCount();
  const replay = new TextEncoder().encode(
    AGENT_REPLAY_LINE.repeat(AGENT_REPLAY_BYTES / AGENT_REPLAY_LINE.length),
  );
  let agentOpens = 0;
  let agentCloses = 0;
  let shellOpens = 0;
  let shellCloses = 0;
  const handle = (agent: boolean): PtyHandle => {
    if (agent) agentOpens += 1;
    else shellOpens += 1;
    return {
      connect: (sink) => queueMicrotask(() => sink({ data: agent ? replay : new Uint8Array(), replay: true })),
      input: () => {},
      resize: () => {},
      onStatus: (listener) => {
        listener({ phase: "online" });
        return () => {};
      },
      ptySize: () => null,
      onPtySize: () => () => {},
      retry: () => {},
      close: () => {
        if (agent) agentCloses += 1;
        else shellCloses += 1;
      },
    };
  };
  const client = {
    openPty: () => handle(true),
    openTerminal: () => handle(false),
    ptyResize: () => {},
    terminalResize: () => {},
    ptyInput: () => {},
    terminalInput: () => {},
  } as unknown as PmClient;

  const host = document.querySelector<HTMLElement>("#host");
  if (!host) throw new Error("missing host");
  const stage = new TerminalStage(client, new TerminalThemeController());
  stage.mount(host);
  stage.show(1n);
  await nextFrame();
  for (let id = 1; id <= SHELL_COUNT; id += 1) {
    stage.showTerminal(BigInt(id));
    await nextFrame();
  }

  const samples: number[] = [];
  for (let index = 0; index < SWITCH_COUNT; index += 1) {
    const started = performance.now();
    if (index % 2 === 0) stage.show(1n);
    else stage.showTerminal(BigInt(SHELL_COUNT));
    samples.push(performance.now() - started);
    await nextFrame();
  }
  samples.sort((a, b) => a - b);
  const percentile = (value: number) => samples[Math.min(samples.length - 1, Math.floor(samples.length * value))];
  const result: ProbeResult = {
    shells: SHELL_COUNT,
    switches: SWITCH_COUNT,
    agentOpens,
    agentCloses,
    shellOpens,
    shellCloses,
    selectionUnmounts,
    showP50Ms: round(percentile(0.5)),
    showP99Ms: round(percentile(0.99)),
    showMaxMs: round(samples[samples.length - 1]),
  };
  stage.disposeAll();
  (window as unknown as { __terminalSwitchResult: ProbeResult }).__terminalSwitchResult = result;
}

void main();
