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
} from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { initialState, type AppState } from "@puppet-master/client-core/state/reducer";
import { ClientContext } from "../src/state/hooks";
import { SessionPane } from "../src/views/SessionPane";
import type { PmClient } from "@puppet-master/client-core/ws/client";
import { createTerminalProbeClient } from "./terminalProbeClient";
import { TerminalStage } from "../src/ws/terminal";
import { TerminalThemeController } from "../src/theme/controller";
import { preloadTerminalFonts } from "../src/fonts";
import "@fontsource-variable/inter";
import "@fontsource-variable/jetbrains-mono";
import "../src/styles.css";

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
const sumPrefix = (counts: Map<string, number>, prefix: string) => [...counts]
  .reduce((total, [key, count]) => total + (key.startsWith(prefix) ? count : 0), 0);
async function waitForReadyLayer(stage: TerminalStage, key: string): Promise<void> {
  const deadline = performance.now() + 180_000;
  while (performance.now() < deadline) {
    const layers = stage.debugSnapshot();
    if (layers.some((layer) => layer.key === key && layer.visible && layer.cols > 1)) return;
    await nextFrame();
  }
  throw new Error(`timed out waiting for Canvas terminal layer ${key}`);
}
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
    ...initialState,
    conn: "online",
    hydrated: true,
    sessions: new Map([["1", session]]),
    terminals: new Map([["1", agent], ["2", shell]]),
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
  await preloadTerminalFonts();
  const selectionUnmounts = await selectionUnmountCount();
  const replay = new TextEncoder().encode(
    AGENT_REPLAY_LINE.repeat(AGENT_REPLAY_BYTES / AGENT_REPLAY_LINE.length),
  );
  const fixture = createTerminalProbeClient((key) => key === "s:1" ? replay : new Uint8Array());
  const client = fixture.client;

  const stage = new TerminalStage(client, new TerminalThemeController());
  const host = document.querySelector<HTMLElement>("#host");
  if (!host) throw new Error("missing host");
  Object.assign(document.body.style, { margin: "0", width: "100vw", height: "100vh" });
  Object.assign(host.style, { position: "relative", width: "100%", height: "100%" });
  stage.mount(host);
  stage.show(1n);
  await nextFrame();
  for (let id = 1; id <= SHELL_COUNT; id += 1) {
    stage.showTerminal(BigInt(id));
    await nextFrame();
  }
  await waitForReadyLayer(stage, `t:${SHELL_COUNT}`);

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
    agentOpens: fixture.opens.get("s:1") ?? 0,
    agentCloses: fixture.closes.get("s:1") ?? 0,
    shellOpens: sumPrefix(fixture.opens, "t:"),
    shellCloses: sumPrefix(fixture.closes, "t:"),
    selectionUnmounts,
    showP50Ms: round(percentile(0.5)),
    showP99Ms: round(percentile(0.99)),
    showMaxMs: round(samples[samples.length - 1]),
  };
  stage.disposeAll();
  (window as unknown as { __terminalSwitchResult: ProbeResult }).__terminalSwitchResult = result;
}

void main();
