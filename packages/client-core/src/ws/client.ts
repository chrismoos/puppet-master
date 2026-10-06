import type { TerminalOwnership } from "./pty";
import { create, fromBinary, toBinary } from "@bufbuild/protobuf";
import {
  ClientMessageSchema,
  CreateBucketSchema,
  AddReviewCommentSchema,
  AdvanceReviewSchema,
  CreateShellSchema,
  DeleteReviewThreadSchema,
  EditReviewCommentSchema,
  FinishReviewSchema,
  OpenReviewSchema,
  ReplyReviewThreadSchema,
  ResolveReviewThreadSchema,
  SendReviewThreadsSchema,
  SetReviewViewerStateSchema,
  type ReviewSide,
  type ReviewChoiceSelect,
  DeleteBucketSchema,
  DeleteProjectSchema,
  CloseForwardSchema,
  CloseTerminalSchema,
  DeleteItemSchema,
  RestartTerminalSchema,
  SnoozeItemSchema,
  UpsertItemSchema,
  InterruptSessionSchema,
  KillSessionSchema,
  ResumeSessionSchema,
  RespondToItemSchema,
  NewSupervisorTargetSchema,
  ServerMessageSchema,
  SpawnSessionSchema,
  UpdateSessionApisSchema,
  UpdateProjectSchema,
  ListInstructionsSchema,
  GetEffectiveInstructionsSchema,
  SetInstructionsSchema,
  RevertInstructionsSchema,
  GetSessionSchema,
  ListEndedSessionsSchema,
  SearchSessionsSchema,
  MarkSessionSeenSchema,
  SetBucketDefaultAgentSchema,
  SetProjectDefaultAgentSchema,
  CreateModelProfileSchema,
  UpdateModelProfileSchema,
  DeleteModelProfileSchema,
  SetModelProfileEndpointSchema,
  DeleteModelProfileEndpointSchema,
  SetBucketModelProfileSchema,
  SetProjectModelProfileSchema,
  type ModelDialect,
  SessionPageSchema,
  SubscribeSchema,
  PermissionMode,
  SessionRole,
  TerminalKind,
  AgentKind,
  type ClientMessage,
  type Session,
  type SessionPage,
  type UpsertItem,
  type InstructionTarget,
  SessionAlertKind,
  type SecurityNotice,
} from "../gen/pm/v1/pm_pb";
import { SOCKET_OPEN, type SocketConnector, type SocketLike } from "../platform";
import type { ItemResponseTarget } from "../state/itemResponse";
import { AlertCatchUp } from "../state/alertCatchUp";
import { initialState, reduce, type Action, type AppState } from "../state/reducer";
import { workerUnavailableReason } from "../state/worker";
import {
  CommandTracker,
  createProjectMsg,
  setBucketPermissionModeMsg,
  setProjectPermissionModeMsg,
  type CommandOutcome,
} from "./commands";
import { PtyBuffer, type PtySink, type TerminalSize } from "./pty";
import { TerminalSocket, type TerminalSocketStats, type TerminalStreamStatus } from "./terminalSocket";

/** Server close code meaning the session cookie is missing or expired. */
export const WS_CLOSE_UNAUTHENTICATED = 4401;

const RECONNECT_MIN_DELAY_MS = 500;
const RECONNECT_MAX_DELAY_MS = 8_000;
const RECONNECT_BACKOFF_FACTOR = 2;

/** How long a manual refresh waits for the reconnect's snapshot before it
 *  reports failure, so the pull control cannot spin forever. */
const REFRESH_TIMEOUT_MS = 15_000;

/** A partial write of review viewer state; omitted fields keep their
 * stored value, so a scroll update never blanks the viewed set. */
/** A review states its own context: the tree it reads and a resolved
 * base SHA. Nothing is inferred from the session, whose launch
 * directory is fixed at spawn while the agent moves. */
export interface OpenReviewInput {
  sessionId: bigint;
  worktree: string;
  base: string;
  head?: string;
  pathspec?: string[];
  files?: string[];
  sourceFile?: string;
  label?: string;
  reset?: boolean;
}

/** An answer to a marked option list, as a caller supplies it. */
export interface ReviewChoiceAnswerInput {
  choiceId: string;
  select: ReviewChoiceSelect;
  optionIds: string[];
  optionLabels: string[];
  otherText: string;
  notes: string;
}

export interface ReviewViewerStateInput {
  reviewId: bigint;
  pinnedRev?: number;
  view?: string;
  layout?: string;
  context?: number;
  viewedFiles?: string[];
  setViewedFiles?: boolean;
  previewOffFiles?: string[];
  setPreviewOffFiles?: boolean;
  lastThreadId?: bigint;
  scrollKey?: string;
  scrollTop?: number;
  fileListCollapsed?: boolean;
  draftKey?: string;
  draftBody?: string;
  seenThread?: bigint;
  seenMessage?: bigint;
}

export interface PtyHandle {
  connect(sink: PtySink): void;
  input(data: Uint8Array, submitted?: boolean): void;
  /** Output bytes the terminal has parsed, which paces a flood to this viewer. */
  ack(bytes: number): void;
  resize(cols: number, rows: number): void;
  onStatus(listener: (status: TerminalStreamStatus) => void): () => void;
  /** The PTY's actual size as last echoed by the daemon, shared by every
   * viewer of the terminal; null until the first echo arrives. */
  ptySize(): TerminalSize | null;
  onPtySize(listener: (size: TerminalSize) => void): () => void;
  onViewerOwnership?(listener: (ownership: TerminalOwnership) => void): () => void;
  stats(): TerminalSocketStats;
  resync(): void;
  /** A fresh snapshot on the open socket, falling back to a reconnect. */
  refreshSnapshot(): void;
  retry(): void;
  close(): void;
}

type ClientMessagePayload = ClientMessage["msg"];

interface SnapshotWaiter {
  settle(): void;
  fail(reason: string): void;
}

export class PmClient {
  onUnauthenticated: (() => void) | null = null;

  private state: AppState = initialState;
  private listeners = new Set<() => void>();
  private alertListeners = new Set<(session: Session, kind: SessionAlertKind) => void>();
  private securityNoticeListeners = new Set<(notice: SecurityNotice) => void>();
  private alertCatchUp = new AlertCatchUp();
  private ws: SocketLike | null = null;
  private tracker = new CommandTracker();
  private terminalSockets = new Map<string, Set<TerminalSocket>>();
  private reconnectDelayMs = RECONNECT_MIN_DELAY_MS;
  private reconnectTimer: ReturnType<typeof setTimeout> | null = null;
  private stopped = true;
  private expiryTimer: ReturnType<typeof setInterval> | null = null;
  private snapshotWaiters = new Set<SnapshotWaiter>();
  private selectedSessionId: string | null = null;

  constructor(private connector: SocketConnector) {}

  /**
   * Connects, abandoning any socket from a previous run. Starting is
   * authoritative rather than idempotent because a connection the OS killed
   * while the app was suspended still reads as open here, so treating the
   * client as already running would leave it with a dead socket and no
   * fresh snapshot.
   */
  start(): void {
    this.stopped = false;
    this.expiryTimer ??= setInterval(() => {
      this.dispatch({ type: "expireSessions", nowUnixMs: Date.now() });
    }, 1_000);
    this.reconnectDelayMs = RECONNECT_MIN_DELAY_MS;
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    const previous = this.ws;
    this.ws = null;
    if (previous) {
      previous.close();
      this.tracker.failAll("reconnecting");
    }
    this.connect();
  }

  /**
   * Reconnects and resolves once the resulting snapshot has been applied,
   * for a manual refresh that has to report a real round trip. It cannot
   * wait on a request over the open socket, because a socket the OS killed
   * accepts sends and never answers.
   */
  refresh(timeoutMs = REFRESH_TIMEOUT_MS): Promise<void> {
    return new Promise((resolve, reject) => {
      let timer: ReturnType<typeof setTimeout> | null = null;
      const waiter: SnapshotWaiter = {
        settle: () => {
          if (timer !== null) clearTimeout(timer);
          this.snapshotWaiters.delete(waiter);
          resolve();
        },
        fail: (reason) => {
          if (timer !== null) clearTimeout(timer);
          this.snapshotWaiters.delete(waiter);
          reject(new Error(reason));
        },
      };
      timer = setTimeout(() => waiter.fail("refresh timed out"), timeoutMs);
      this.snapshotWaiters.add(waiter);
      this.start();
    });
  }

  stop(): void {
    this.stopped = true;
    if (this.expiryTimer !== null) {
      clearInterval(this.expiryTimer);
      this.expiryTimer = null;
    }
    if (this.reconnectTimer !== null) {
      clearTimeout(this.reconnectTimer);
      this.reconnectTimer = null;
    }
    const ws = this.ws;
    this.ws = null;
    ws?.close();
    for (const sockets of this.terminalSockets.values()) {
      for (const socket of sockets) socket.close();
    }
    this.terminalSockets.clear();
    this.tracker.failAll("client stopped");
    for (const waiter of [...this.snapshotWaiters]) waiter.fail("client stopped");
  }

  getState = (): AppState => this.state;

  subscribe = (listener: () => void): (() => void) => {
    this.listeners.add(listener);
    return () => this.listeners.delete(listener);
  };

  /**
   * Fires when the daemon raises an alert for a session this client knows.
   * The daemon decides what is alert-worthy, using the same classification
   * that gates push, so this client renders rather than re-deriving.
   */
  onAlert = (listener: (session: Session, kind: SessionAlertKind) => void): (() => void) => {
    this.alertListeners.add(listener);
    return () => this.alertListeners.delete(listener);
  };

  /**
   * Fires when the daemon says something granted or moved a credential: a
   * device or host enrolled, a host's key replaced, a session rewriting the
   * standing instructions. The API mints rather than reads back, so the thing
   * worth telling the user about is what was created, and revocation is the
   * only undo.
   */
  onSecurityNotice = (listener: (notice: SecurityNotice) => void): (() => void) => {
    this.securityNoticeListeners.add(listener);
    return () => this.securityNoticeListeners.delete(listener);
  };

  request(msg: ClientMessagePayload): Promise<CommandOutcome> {
    if (this.ws?.readyState !== SOCKET_OPEN) {
      return Promise.reject(new Error("not connected"));
    }
    const seq = this.tracker.nextSeq();
    const result = this.tracker.register(seq);
    this.ws.send(toBinary(ClientMessageSchema, create(ClientMessageSchema, { seq, msg })));
    return result;
  }

  async getSession(sessionId: bigint): Promise<Session> {
    const outcome = await this.request({
      case: "getSession",
      value: create(GetSessionSchema, { sessionId }),
    });
    const page = decodeSessionPage(outcome.data);
    if (!page.sessions[0]) throw new Error(`session ${sessionId} not found`);
    return page.sessions[0];
  }

  async listEndedSessions(cursor = ""): Promise<SessionPage> {
    const outcome = await this.request({
      case: "listEndedSessions",
      value: create(ListEndedSessionsSchema, { cursor, limit: 50 }),
    });
    return decodeSessionPage(outcome.data);
  }

  async searchSessions(query: string, cursor = ""): Promise<SessionPage> {
    const outcome = await this.request({
      case: "searchSessions",
      value: create(SearchSessionsSchema, { query, cursor, limit: 50 }),
    });
    return decodeSessionPage(outcome.data);
  }

  ownSessionPage(source: "history" | "search", sessions: readonly Session[], replace: boolean): void {
    this.dispatch({ type: "sessionPage", source, sessions, replace });
  }

  async retainSelectedSession(id: string | null): Promise<boolean> {
    this.selectedSessionId = id;
    this.dispatch({ type: "selectedSession", id });
    if (!id || this.state.sessions.has(id)) return true;
    try {
      const session = await this.getSession(BigInt(id));
      if (this.selectedSessionId === id) this.dispatch({ type: "selectedSession", id, session });
      return true;
    } catch {
      return false;
    }
  }

  markSessionSeen(sessionId: bigint): Promise<CommandOutcome> {
    return this.request({
      case: "markSessionSeen",
      value: create(MarkSessionSeenSchema, { sessionId }),
    });
  }

  spawnSession(
    projectId: bigint,
    agent: AgentKind | undefined,
    taskTitle: string,
    taskPrompt: string,
    cwd = "",
    permissionMode: PermissionMode = PermissionMode.UNSPECIFIED,
    workerId?: bigint,
    itemsApi = true,
    supervisorApi = false,
    role: SessionRole = SessionRole.WORKER,
    modelProfileId?: bigint,
    initialCols?: number,
    initialRows?: number,
  ): Promise<CommandOutcome> {
    return this.request({
      case: "spawnSession",
      value: create(SpawnSessionSchema, {
        projectId,
        agent: agent ?? AgentKind.UNSPECIFIED,
        taskTitle,
        taskPrompt,
        cwd,
        permissionMode,
        workerId,
        itemsApi,
        supervisorApi,
        role,
        modelProfileId,
        initialCols: initialCols !== undefined && initialCols >= 2 ? initialCols : undefined,
        initialRows: initialRows !== undefined && initialRows >= 1 ? initialRows : undefined,
      }),
    });
  }

  /** Human item write; absent fields leave the stored value untouched. */
  upsertItem(write: Partial<Omit<UpsertItem, "$typeName">>): Promise<CommandOutcome> {
    return this.request({ case: "upsertItem", value: create(UpsertItemSchema, write) });
  }

  /** Toggles a session's MCP APIs; absent fields leave the stored value untouched. */
  updateSessionApis(
    sessionId: bigint,
    apis: { itemsApi?: boolean; supervisorApi?: boolean; role?: SessionRole },
  ): Promise<CommandOutcome> {
    return this.request({
      case: "updateSessionApis",
      value: create(UpdateSessionApisSchema, {
        sessionId,
        itemsApi: apis.itemsApi,
        supervisorApi: apis.supervisorApi,
        role: apis.role,
      }),
    });
  }

  listInstructions(bucketId: bigint, projectId?: bigint) { return this.request({case:"listInstructions",value:create(ListInstructionsSchema,{bucketId,projectId})}); }
  getEffectiveInstructions(bucketId:bigint,role:SessionRole,projectId?:bigint){return this.request({case:"getEffectiveInstructions",value:create(GetEffectiveInstructionsSchema,{bucketId,role,projectId})});}
  setInstructions(bucketId:bigint,projectId:bigint|undefined,target:InstructionTarget,markdown:string,expectedRevision:bigint,note:string){return this.request({case:"setInstructions",value:create(SetInstructionsSchema,{bucketId,projectId,target,markdown,expectedRevision,note})});}
  revertInstructions(layerId:bigint,revision:bigint,expectedRevision:bigint,note:string){return this.request({case:"revertInstructions",value:create(RevertInstructionsSchema,{layerId,revision,expectedRevision,note})});}

  /** Records a free-form reply to an item's question and routes it to a session. */
  respondToItem(
    bucketId: bigint,
    itemId: bigint,
    text: string,
    target: ItemResponseTarget,
  ): Promise<CommandOutcome> {
    const wire =
      target.kind === "session"
        ? { case: "sessionId" as const, value: target.sessionId }
        : target.kind === "newSupervisor"
          ? {
              case: "newSupervisor" as const,
              value: create(NewSupervisorTargetSchema, { projectId: target.projectId }),
            }
          : { case: "replyOnly" as const, value: true };
    return this.request({
      case: "respondToItem",
      value: create(RespondToItemSchema, { bucketId, itemId, text, target: wire }),
    });
  }

  deleteItem(bucketId: bigint, id: bigint): Promise<CommandOutcome> {
    return this.request({ case: "deleteItem", value: create(DeleteItemSchema, { bucketId, id }) });
  }

  /** Parks an item until a time; omit to clear the snooze. */
  snoozeItem(bucketId: bigint, id: bigint, untilUnixMs?: bigint): Promise<CommandOutcome> {
    return this.request({ case: "snoozeItem", value: create(SnoozeItemSchema, { bucketId, id, untilUnixMs }) });
  }

  setBucketPermissionMode(bucketId: bigint, mode: PermissionMode): Promise<CommandOutcome> {
    return this.request(setBucketPermissionModeMsg(bucketId, mode));
  }

  setProjectPermissionMode(projectId: bigint, mode: PermissionMode): Promise<CommandOutcome> {
    return this.request(setProjectPermissionModeMsg(projectId, mode));
  }

  setBucketDefaultAgent(bucketId: bigint, agent?: AgentKind): Promise<CommandOutcome> {
    return this.request({
      case: "setBucketDefaultAgent",
      value: create(SetBucketDefaultAgentSchema, { bucketId, agent: agent ?? AgentKind.UNSPECIFIED }),
    });
  }

  setProjectDefaultAgent(projectId: bigint, agent?: AgentKind): Promise<CommandOutcome> {
    return this.request({
      case: "setProjectDefaultAgent",
      value: create(SetProjectDefaultAgentSchema, { projectId, agent: agent ?? AgentKind.UNSPECIFIED }),
    });
  }

  createModelProfile(name: string, apiKey?: string): Promise<CommandOutcome> {
    return this.request({
      case: "createModelProfile",
      value: create(CreateModelProfileSchema, { name, apiKey }),
    });
  }

  /** Omitted fields keep their stored value, so an edit never drops the key. */
  updateModelProfile(
    id: bigint,
    changes: { name?: string; apiKey?: string; clearApiKey?: boolean },
  ): Promise<CommandOutcome> {
    return this.request({
      case: "updateModelProfile",
      value: create(UpdateModelProfileSchema, {
        id,
        name: changes.name,
        apiKey: changes.apiKey,
        clearApiKey: changes.clearApiKey ?? false,
      }),
    });
  }

  deleteModelProfile(id: bigint): Promise<CommandOutcome> {
    return this.request({
      case: "deleteModelProfile",
      value: create(DeleteModelProfileSchema, { id }),
    });
  }

  setModelProfileEndpoint(
    profileId: bigint,
    dialect: ModelDialect,
    model: string,
    baseUrl: string,
    backgroundModel: string,
  ): Promise<CommandOutcome> {
    return this.request({
      case: "setModelProfileEndpoint",
      value: create(SetModelProfileEndpointSchema, {
        profileId,
        dialect,
        model,
        baseUrl,
        backgroundModel,
      }),
    });
  }

  deleteModelProfileEndpoint(profileId: bigint, dialect: ModelDialect): Promise<CommandOutcome> {
    return this.request({
      case: "deleteModelProfileEndpoint",
      value: create(DeleteModelProfileEndpointSchema, { profileId, dialect }),
    });
  }

  setBucketModelProfile(bucketId: bigint, modelProfileId?: bigint): Promise<CommandOutcome> {
    return this.request({
      case: "setBucketModelProfile",
      value: create(SetBucketModelProfileSchema, { bucketId, modelProfileId }),
    });
  }

  setProjectModelProfile(projectId: bigint, modelProfileId?: bigint): Promise<CommandOutcome> {
    return this.request({
      case: "setProjectModelProfile",
      value: create(SetProjectModelProfileSchema, { projectId, modelProfileId }),
    });
  }

  interruptSession(sessionId: bigint): Promise<CommandOutcome> {
    return this.request({
      case: "interruptSession",
      value: create(InterruptSessionSchema, { sessionId }),
    });
  }

  killSession(sessionId: bigint): Promise<CommandOutcome> {
    return this.request({
      case: "killSession",
      value: create(KillSessionSchema, { sessionId }),
    });
  }

  resumeSession(sessionId: bigint): Promise<CommandOutcome> {
    return this.request({
      case: "resumeSession",
      value: create(ResumeSessionSchema, { sessionId }),
    });
  }

  openReview(input: OpenReviewInput): Promise<CommandOutcome> {
    return this.request({ case: "openReview", value: create(OpenReviewSchema, input) });
  }
  addReviewComment(input: { reviewId: bigint; path: string; line: number; side: ReviewSide; excerpt: string; body: string; send: boolean; anchorSnapshotId?: bigint; choice?: ReviewChoiceAnswerInput }): Promise<CommandOutcome> {
    return this.request({ case: "addReviewComment", value: create(AddReviewCommentSchema, input) });
  }
  editReviewComment(messageId: bigint, body: string, choice?: ReviewChoiceAnswerInput): Promise<CommandOutcome> {
    return this.request({ case: "editReviewComment", value: create(EditReviewCommentSchema, { messageId, body, choice }) });
  }
  deleteReviewThread(threadId: bigint): Promise<CommandOutcome> {
    return this.request({ case: "deleteReviewThread", value: create(DeleteReviewThreadSchema, { threadId }) });
  }
  sendReviewThreads(reviewId: bigint, threadIds: bigint[] = []): Promise<CommandOutcome> {
    return this.request({ case: "sendReviewThreads", value: create(SendReviewThreadsSchema, { reviewId, threadIds }) });
  }
  replyReviewThread(threadId: bigint, body: string): Promise<CommandOutcome> {
    return this.request({ case: "replyReviewThread", value: create(ReplyReviewThreadSchema, { threadId, body }) });
  }

  resolveReviewThread(threadId: bigint, resolved: boolean): Promise<CommandOutcome> {
    return this.request({ case: "resolveReviewThread", value: create(ResolveReviewThreadSchema, { threadId, resolved }) });
  }
  advanceReview(reviewId: bigint, rev = 0): Promise<CommandOutcome> {
    return this.request({ case: "advanceReview", value: create(AdvanceReviewSchema, { reviewId, rev }) });
  }
  finishReview(reviewId: bigint): Promise<CommandOutcome> {
    return this.request({ case: "finishReview", value: create(FinishReviewSchema, { reviewId }) });
  }
  setReviewViewerState(input: ReviewViewerStateInput): Promise<CommandOutcome> {
    return this.request({ case: "setReviewViewerState", value: create(SetReviewViewerStateSchema, input) });
  }
  createShell(sessionId: bigint, title = ""): Promise<CommandOutcome> { return this.request({ case: "createShell", value: create(CreateShellSchema, { sessionId, title }) }); }
  restartTerminal(terminalId: bigint): Promise<CommandOutcome> { return this.request({ case: "restartTerminal", value: create(RestartTerminalSchema, { terminalId }) }); }
  closeTerminal(terminalId: bigint): Promise<CommandOutcome> { return this.request({ case: "closeTerminal", value: create(CloseTerminalSchema, { terminalId }) }); }
  closeForward(forwardId: bigint): Promise<CommandOutcome> { return this.request({ case: "closeForward", value: create(CloseForwardSchema, { forwardId }) }); }
  createBucket(name: string, allowedWorkerIds: bigint[], defaultWorkerId: bigint, isDefault = false): Promise<CommandOutcome> {
    return this.request({
      case: "createBucket",
      value: create(CreateBucketSchema, { name, allowedWorkerIds, defaultWorkerId, isDefault }),
    });
  }

  deleteBucket(id: bigint): Promise<CommandOutcome> {
    return this.request({
      case: "deleteBucket",
      value: create(DeleteBucketSchema, { id }),
    });
  }

  createProject(
    bucketId: bigint,
    name: string,
    path: string,
    workerId?: bigint,
    allowedWorkerIds: bigint[] = [],
  ): Promise<CommandOutcome> {
    return this.request(createProjectMsg(bucketId, name, path, workerId, allowedWorkerIds));
  }

  deleteProject(id: bigint): Promise<CommandOutcome> {
    return this.request({
      case: "deleteProject",
      value: create(DeleteProjectSchema, { id }),
    });
  }

  updateProject(id: bigint, path: string): Promise<CommandOutcome> {
    return this.request({
      case: "updateProject",
      value: create(UpdateProjectSchema, { projectId: id, path }),
    });
  }

  openPty(sessionId: bigint, initialSize?: TerminalSize | null): PtyHandle {
    const terminal = [...this.state.terminals.values()].find(
      (item) => item.sessionId === sessionId && item.kind === TerminalKind.AGENT,
    );
    if (!terminal) throw new Error(`session ${sessionId} has no agent terminal`);
    return this.createTerminalHandle(terminal.id, terminal.generation, initialSize);
  }

  openTerminal(terminalId: bigint, initialSize?: TerminalSize | null): PtyHandle {
    const generation = this.state.terminals.get(terminalId.toString())?.generation ?? 0n;
    return this.createTerminalHandle(terminalId, generation, initialSize);
  }

  private createTerminalHandle(
    terminalId: bigint,
    generation: bigint,
    initialSize?: TerminalSize | null,
  ): PtyHandle {
    const buffer = new PtyBuffer();
    const unavailableReason = this.terminalWorkerUnavailableReason(terminalId);
    const socket = new TerminalSocket(
      this.connector,
      terminalId,
      generation,
      (frame) => buffer.push(frame),
      () => this.onUnauthenticated?.(),
      unavailableReason,
      initialSize,
    );
    const key = terminalId.toString();
    const sockets = this.terminalSockets.get(key) ?? new Set<TerminalSocket>();
    sockets.add(socket);
    this.terminalSockets.set(key, sockets);
    return {
      connect: (sink) => buffer.connect(sink),
      input: (data, submitted) => socket.input(data, submitted),
      ack: (bytes) => socket.ack(bytes),
      resize: (cols, rows) => socket.resize(cols, rows),
      onStatus: (listener) => socket.subscribe(listener),
      ptySize: () => socket.ptySize(),
      onPtySize: (listener) => socket.onPtySize(listener),
      onViewerOwnership: (listener) => socket.onViewerOwnership(listener),
      stats: () => socket.stats(),
      resync: () => socket.resync(),
      refreshSnapshot: () => socket.refreshSnapshot(),
      retry: () => socket.retry(),
      close: () => {
        socket.close();
        sockets.delete(socket);
        if (sockets.size === 0) this.terminalSockets.delete(key);
      },
    };
  }

  private terminalWorkerUnavailableReason(terminalId: bigint): string | null {
    const terminal = this.state.terminals.get(terminalId.toString());
    if (!terminal) return null;
    const session = this.state.sessions.get(terminal.sessionId.toString());
    if (!session) return null;
    return workerUnavailableReason(this.state.workers, session.workerId);
  }

  private syncTerminalGenerations(): void {
    for (const [key, sockets] of this.terminalSockets) {
      const terminal = this.state.terminals.get(key);
      if (!terminal) continue;
      for (const socket of sockets) {
        socket.updateGeneration(terminal.generation);
      }
    }
  }

  private syncTerminalAvailability(): void {
    for (const [key, sockets] of this.terminalSockets) {
      const terminal = this.state.terminals.get(key);
      if (!terminal) continue;
      const unavailableReason = this.terminalWorkerUnavailableReason(terminal.id);
      for (const socket of sockets) {
        if (unavailableReason) socket.setAvailable(false, unavailableReason);
        else socket.setAvailable(true);
      }
    }
  }

  private emitAlert(session: Session, kind: SessionAlertKind): void {
    for (const listener of this.alertListeners) listener(session, kind);
  }

  private emitSecurityNotice(notice: SecurityNotice): void {
    for (const listener of this.securityNoticeListeners) listener(notice);
  }

  private dispatch(action: Action): void {
    this.state = reduce(this.state, action);
    for (const listener of this.listeners) {
      listener();
    }
  }

  private connect(): void {
    const ws = this.connector("/ws");
    ws.binaryType = "arraybuffer";
    this.ws = ws;
    this.dispatch({ type: "conn", phase: "connecting" });

    ws.onopen = () => {
      if (this.ws !== ws) return;
      this.reconnectDelayMs = RECONNECT_MIN_DELAY_MS;
      this.dispatch({ type: "conn", phase: "online" });
      this.request({
        case: "subscribe",
        value: create(SubscribeSchema, {
          scope: { scope: { case: "all", value: true } },
        }),
      }).catch((err: unknown) => {
        console.warn("subscribe failed", err);
      });
    };

    ws.onmessage = (ev) => {
      if (this.ws !== ws) return;
      this.handleFrame(new Uint8Array(ev.data as ArrayBuffer));
    };

    ws.onclose = (ev) => {
      if (this.ws !== ws) return;
      this.ws = null;
      this.tracker.failAll("connection lost");
      this.dispatch({ type: "conn", phase: "offline" });
      if (ev.code === WS_CLOSE_UNAUTHENTICATED) {
        this.onUnauthenticated?.();
        return;
      }
      if (!this.stopped) {
        this.scheduleReconnect();
      }
    };
  }

  private scheduleReconnect(): void {
    this.reconnectTimer = setTimeout(() => {
      this.reconnectTimer = null;
      if (!this.stopped) this.connect();
    }, this.reconnectDelayMs);
    this.reconnectDelayMs = Math.min(
      this.reconnectDelayMs * RECONNECT_BACKOFF_FACTOR,
      RECONNECT_MAX_DELAY_MS,
    );
  }

  private handleFrame(bytes: Uint8Array): void {
    const msg = fromBinary(ServerMessageSchema, bytes);
    switch (msg.msg.case) {
      case "snapshot": {
        const previous = this.state.hydrated ? this.state.sessions : null;
        this.dispatch({ type: "snapshot", snapshot: msg.msg.value });
        this.syncTerminalGenerations();
        this.syncTerminalAvailability();
        for (const { session, kind } of this.alertCatchUp.snapshot(previous, this.state.sessions.values())) {
          this.emitAlert(session, kind);
        }
        for (const waiter of [...this.snapshotWaiters]) waiter.settle();
        break;
      }
      case "event": {
        this.dispatch({ type: "event", event: msg.msg.value });
        if (msg.msg.value.event.case === "terminalChanged") this.syncTerminalGenerations();
        if (msg.msg.value.event.case === "workerChanged"
          || msg.msg.value.event.case === "workerRemoved") {
          this.syncTerminalAvailability();
        }
        if (msg.msg.value.event.case === "sessionChanged") {
          this.alertCatchUp.observe(msg.msg.value.event.value);
        }
        if (msg.msg.value.event.case === "sessionAlert") {
          const alert = msg.msg.value.event.value;
          this.alertCatchUp.noteAlert(alert.sessionId, alert.kind);
          const session = this.state.sessions.get(alert.sessionId.toString());
          if (session) this.emitAlert(session, alert.kind);
        }
        if (msg.msg.value.event.case === "securityNotice") {
          this.emitSecurityNotice(msg.msg.value.event.value);
        }
        break;
      }
      case "commandResult":
        this.tracker.settle(msg.msg.value);
        break;
      case undefined:
        break;
    }
  }
}

function decodeSessionPage(data?: Uint8Array): SessionPage {
  // A valid zero-result SessionPage is protobuf's empty message, so the
  // command tracker legitimately exposes it without a data field.
  return fromBinary(SessionPageSchema, data ?? new Uint8Array());
}
