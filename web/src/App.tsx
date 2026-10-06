import { openForward, parseForwardHandoff } from "./views/forwardLink";
import { ownsItsWindow, reviewEntry } from "./views/reviewWindow";
import { FullscreenReview } from "./views/FullscreenReview";
import { browserJsonFetch } from "./api/http";
import {
  useCallback,
  useEffect,
  useLayoutEffect,
  useReducer,
  useRef,
  useState,
  type CSSProperties,
  type PointerEvent as ReactPointerEvent,
} from "react";
import { fetchMe, fetchVersion, logout, type BuildInfo } from "./api/auth";
import { securityNoticeTitle } from "./securityNotice";
import { fetchItem } from "./api/items";
import { createWorkspace, deleteWorkspace, listWorkspaces, reorderWorkspaces, updateWorkspace, type SavedWorkspace } from "./api/workspaces";
import { sessionEnded } from "@puppet-master/client-core/format";
import { visibleSessions } from "@puppet-master/client-core/state/sidebar";
import { sessionRoutePath, settingsRoutePath, withRememberedTab, type SettingsSection } from "@puppet-master/client-core/router";
import { navigate, sessionHomeId, useRoute } from "./router";
import { Notifier } from "./notify/notifier";
import { browserSocketConnector } from "./ws/browserSocket";
import {
  needsInputNotification,
  sessionAlertNotification,
  shouldAutoRequestNotify,
} from "@puppet-master/client-core/state/notify";
import {
  boardReturnPath,
  rememberBoardView,
  restorableView,
  validBoardView,
  type BoardViewInventory,
  type BoardViewMemory,
} from "@puppet-master/client-core/state/boardNavigation";
import {
  SETTINGS_RETURN_TARGET_KEY,
  settingsEntryReturnTarget,
  settingsReturnPath,
  settingsReturnView,
  parseSettingsReturnTarget,
  serializeSettingsReturnTarget,
  type SettingsReturnInventory,
} from "@puppet-master/client-core/state/settingsNavigation";
import {
  sessionFallbackPath,
  selectedSessionBecameUnavailable,
} from "@puppet-master/client-core/state/sessionNavigation";
import { agentTerminalForSession, parseWorkspaceLayout, removeTerminalFromLayout } from "@puppet-master/client-core/state/workspace";
import {
  SessionAlertKind,
  type SecurityNotice,
  TerminalKind,
  type Item,
  type Session,
  type Terminal,
} from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { ClientContext, useAppState, useClient } from "./state/hooks";
import { focusModeActionForKey, focusModeReducer } from "@puppet-master/client-core/state/focusMode";
import {
  NOTIFY_ASKED_KEY,
  NOTIFY_ENABLED_KEY,
  NOTIFY_SOUND_KEY,
  readBool,
  readSidebarWidth,
  readSessionString,
  readString,
  removeLegacyNavigationPreferences,
  SELECTED_SESSION_KEY,
  SHOW_ENDED_KEY,
  SIDEBAR_MAX_WIDTH,
  SIDEBAR_MIN_WIDTH,
  SIDEBAR_WIDTH_KEY,
  writeBool,
  writeSessionString,
  writeString,
} from "./storage";
import { startWebActivityReporting } from "./api/webActivity";
import { startColumnResize } from "./columnResize";
import { ErrorBoundary } from "./components/ErrorBoundary";
import { Popover } from "./components/Popover";
import { ProductBrand } from "./components/ProductBrand";
import { resolvePmLink, type PmLink } from "@puppet-master/client-core/pmlink";
import { AppearanceToggle } from "./views/AppearanceToggle";
import { BoardPage } from "./views/BoardPage";
import { LoginPage } from "./views/LoginPage";
import { SettingsPage } from "./views/SettingsPage";
import { SessionPane, planTabId, sessionTabHref } from "./views/SessionPane";
import { SetupPage } from "./views/SetupPage";
import { parseAppearance, USER_APPEARANCE_KEY } from "@puppet-master/client-core/theme/appearance";
import { parseUiTheme, USER_UI_THEME_KEY } from "@puppet-master/client-core/theme/uiTheme";
import {
  Sidebar,
  StatusTallies,
  needsInputCount,
  rollupCounts,
  type NotifyControls,
  type RollupState,
} from "./views/Sidebar";
import { itemSpawnPrompt } from "@puppet-master/client-core/state/itemSpawnPrompt";
import { itemKey } from "@puppet-master/client-core/state/reducer";
import { SpawnDialog, type SpawnPrefill } from "./views/SpawnDialog";
import { WorkspacePage } from "./views/WorkspacePage";
import { WorkspaceDialog } from "./views/WorkspaceDialog";
import { WorkspaceDeleteDialog } from "./views/WorkspaceDeleteDialog";
import { ConnectionSessionPanel } from "./views/ConnectionsPanel";
import { ReviewPage } from "./views/ReviewPage";
import { ApprovalsButton, ApprovalsPage } from "./views/ApprovalsPage";
import { usePendingApprovals } from "./state/pendingApprovals";
import { approvalRoutePath } from "@puppet-master/client-core/router";
import { removeWorkspaceFromTabs, WorkspaceTabs } from "./views/WorkspaceTabs";
import { PmClient } from "@puppet-master/client-core/ws/client";
import { TerminalStage } from "./ws/terminal";
import { releaseEndedOrRemovedSessions, releaseRemovedTerminals } from "@puppet-master/client-core/ws/terminalLifecycle";
import { TERMINAL_PM_LINK_ERROR_EVENT } from "./ws/terminalLinks";
import { TerminalThemeController } from "./theme/controller";
import { TerminalThemeContext } from "./theme/context";
import {
  USER_TERMINAL_THEME_KEY,
  validateTerminalTheme,
} from "@puppet-master/client-core/theme/terminalTheme";

const APP_TITLE = "Puppet Master";

type AuthPhase =
  | { kind: "loading" }
  | { kind: "setup" }
  | { kind: "login" }
  | { kind: "ready"; username: string }
  | { kind: "error"; message: string };

export function App() {
  const [phase, setPhase] = useState<AuthPhase>({ kind: "loading" });

  useEffect(() => {
    let cancelled = false;
    fetchMe()
      .then((me) => {
        if (cancelled) return;
        if (me.kind === "user") setPhase({ kind: "ready", username: me.username });
        else if (me.kind === "setup") setPhase({ kind: "setup" });
        else setPhase({ kind: "login" });
      })
      .catch((err: unknown) => {
        if (!cancelled) setPhase({ kind: "error", message: String(err) });
      });
    return () => {
      cancelled = true;
    };
  }, []);

  const toLogin = useCallback(() => setPhase({ kind: "login" }), []);
  const toReady = useCallback((username: string) => setPhase({ kind: "ready", username }), []);

  switch (phase.kind) {
    case "loading":
      return <div className="auth-screen" />;
    case "error":
      return (
        <div className="auth-screen">
          <div className="auth-card">
            <ProductBrand />
            <p className="form-error">{phase.message}</p>
          </div>
        </div>
      );
    case "setup":
      return <SetupPage onDone={toReady} />;
    case "login":
      return <LoginPage onDone={toReady} />;
    case "ready":
      return location.hash.startsWith("#/forward-open?")
        ? <ForwardHandoff onUnauthenticated={toLogin} />
        : <ConnectedApp username={phase.username} onUnauthenticated={toLogin} />;
  }
}

function ForwardHandoff({ onUnauthenticated }: { onUnauthenticated: () => void }) {
  const [error, setError] = useState<string | null>(null);
  useEffect(() => {
    let active = true;
    const run = async () => {
      try {
        const handoff = parseForwardHandoff(location.hash);
        if (!handoff) throw new Error("Invalid forward destination");
        await openForward(browserJsonFetch, () => ({
          navigate: (url) => { if (active) location.replace(url); },
          close: () => {},
        }), handoff.id, handoff.destination, true);
      } catch (err) {
        if (!active) return;
        if (err instanceof Error && err.message === "not authenticated") onUnauthenticated();
        else setError(String(err));
      }
    };
    void run();
    return () => { active = false; };
  }, [onUnauthenticated]);
  return <div className="auth-screen"><div className="auth-card">
    <ProductBrand /><p className={error ? "form-error" : undefined}>{error ?? "Redirecting to forward…"}</p>
  </div></div>;
}

function BuildStamp() {
  const [build, setBuild] = useState<BuildInfo | null>(null);
  useEffect(() => {
    let live = true;
    void fetchVersion().then((b) => {
      if (live) setBuild(b);
    });
    return () => {
      live = false;
    };
  }, []);
  if (!build) return null;
  return (
    <span className="build-stamp" title={`version ${build.version}`}>
      v{build.version} · {build.gitRev}
    </span>
  );
}

function ConnectedApp({
  username,
  onUnauthenticated,
}: {
  username: string;
  onUnauthenticated: () => void;
}) {
  const clientRef = useRef<PmClient | null>(null);
  clientRef.current ??= new PmClient(browserSocketConnector);
  const client = clientRef.current;
  const route = useRoute();
  const review = reviewEntry(route);
  const reviewOwnsWindow = useRef(ownsItsWindow(route, window.opener)).current;

  const themeControllerRef = useRef<TerminalThemeController | null>(null);
  themeControllerRef.current ??= new TerminalThemeController();
  const themeController = themeControllerRef.current;

  const stageRef = useRef<TerminalStage | null>(null);
  if (!review) stageRef.current ??= new TerminalStage(client, themeController);
  const stage = stageRef.current;

  const notifierRef = useRef<Notifier | null>(null);
  if (!review) notifierRef.current ??= new Notifier();
  const notifier = notifierRef.current;

  useEffect(() => {
    client.onUnauthenticated = onUnauthenticated;
    client.start();
    return () => {
      client.stop();
      stageRef.current?.disposeAll();
    };
  }, [client, onUnauthenticated]);

  useEffect(() => startWebActivityReporting(), []);

  return (
    <ClientContext.Provider value={client}>
      <TerminalThemeContext.Provider value={themeController}>
        <ConnectedAppearance themeController={themeController} />
        {review ? (
          <FullscreenReview route={review} ownsWindow={reviewOwnsWindow} />
        ) : (
          <Shell
            username={username}
            onLoggedOut={onUnauthenticated}
            stage={stage!}
            notifier={notifier!}
          />
        )}
      </TerminalThemeContext.Provider>
    </ClientContext.Provider>
  );
}

function ConnectedAppearance({ themeController }: { themeController: TerminalThemeController }) {
  const state = useAppState();
  const appearance = parseAppearance(state.userSettings.get(USER_APPEARANCE_KEY));
  useEffect(() => {
    if (appearance === null) delete document.documentElement.dataset.appearance;
    else document.documentElement.dataset.appearance = appearance;
  }, [appearance]);

  const uiTheme = parseUiTheme(state.userSettings.get(USER_UI_THEME_KEY));
  useEffect(() => {
    if (uiTheme === "standard") delete document.documentElement.dataset.theme;
    else document.documentElement.dataset.theme = uiTheme;
  }, [uiTheme]);

  const storedTerminalTheme = state.userSettings.get(USER_TERMINAL_THEME_KEY);
  useEffect(() => {
    if (!storedTerminalTheme) {
      themeController.setPersisted(null);
      return;
    }
    try {
      themeController.setPersisted(validateTerminalTheme(JSON.parse(storedTerminalTheme)));
    } catch (reason) {
      console.error("invalid synchronized terminal theme; using built-in fallback", reason);
      themeController.setPersisted(null);
    }
  }, [storedTerminalTheme, themeController]);

  return null;
}

function Shell({
  username,
  onLoggedOut,
  stage,
  notifier,
}: {
  username: string;
  onLoggedOut: () => void;
  stage: TerminalStage;
  notifier: Notifier;
}) {
  const client = useClient();
  const route = useRoute();
  const state = useAppState();
  const appearance = parseAppearance(state.userSettings.get(USER_APPEARANCE_KEY));
  const isReviewOrPlanTab =
    route.name === "session" &&
    Boolean(route.tab?.startsWith("review:") || route.tab?.startsWith("plan:"));
  const [persistedFocusMode, updateFocusMode] = useReducer(
    focusModeReducer,
    route.name === "session" ? Boolean(route.focus || isReviewOrPlanTab) : false,
  );
  const focusMode =
    route.name === "session"
      ? Boolean(route.focus || isReviewOrPlanTab)
      : persistedFocusMode;
  const reviewFinished = () => updateFocusMode("exit");
  const [spawn, setSpawn] = useState<{
    projectId?: string;
    bucketId?: string;
    lock: boolean;
    prefill?: SpawnPrefill;
  } | null>(null);
  const [settingsBucket, setSettingsBucket] = useState<string | null>(null);
  const [showEnded, setShowEnded] = useState(() => readBool(SHOW_ENDED_KEY, true));
  const [notifyEnabled, setNotifyEnabled] = useState(() => readBool(NOTIFY_ENABLED_KEY, false));
  const [notifySound, setNotifySound] = useState(() => readBool(NOTIFY_SOUND_KEY, false));
  const [notifyPermission, setNotifyPermission] = useState<NotificationPermission>(
    notifier.permission,
  );
  const [sidebarWidth, setSidebarWidth] = useState(() => readSidebarWidth());
  const [resizing, setResizing] = useState(false);
  const [workspaces, setWorkspaces] = useState<SavedWorkspace[]>([]);
  const [workspaceError, setWorkspaceError] = useState<string | null>(null);
  const [routedItem, setRoutedItem] = useState<Item | undefined>();
  const [routedItemLoading, setRoutedItemLoading] = useState(false);
  const [routedItemError, setRoutedItemError] = useState<string | null>(null);
  useEffect(() => {
    const onTerminalLinkError = (event: Event) => {
      setWorkspaceError((event as CustomEvent<string>).detail);
    };
    window.addEventListener(TERMINAL_PM_LINK_ERROR_EVENT, onTerminalLinkError);
    return () => window.removeEventListener(TERMINAL_PM_LINK_ERROR_EVENT, onTerminalLinkError);
  }, []);
  const [workspaceTerminal, setWorkspaceTerminal] = useState<string | undefined>();
  const [workspaceDialog, setWorkspaceDialog] = useState<{ kind: "create" } | { kind: "rename"; workspace: SavedWorkspace } | null>(null);
  const [workspaceDelete, setWorkspaceDelete] = useState<SavedWorkspace | null>(null);
  const [accountMenuOpen, setAccountMenuOpen] = useState(false);
  const [homeSessionId, setHomeSessionId] = useState<string | null>(() => readString(SELECTED_SESSION_KEY));
  const [boardViews, setBoardViews] = useState<BoardViewMemory>(() => new Map());
  const [settingsReturnTarget, setSettingsReturnTarget] = useState(() =>
    parseSettingsReturnTarget(readSessionString(SETTINGS_RETURN_TARGET_KEY)),
  );

  const startResize = useCallback((e: ReactPointerEvent) => {
    e.preventDefault();
    startColumnResize({
      originX: 0,
      min: SIDEBAR_MIN_WIDTH,
      max: SIDEBAR_MAX_WIDTH,
      start: readSidebarWidth(),
      onWidth: setSidebarWidth,
      onCommit: (width) => writeString(SIDEBAR_WIDTH_KEY, String(width)),
      onDragging: setResizing,
    });
  }, []);

  const selectedId = sessionHomeId(route, homeSessionId);
  const [stateFilter, setStateFilter] = useState<RollupState | null>(null);
  const selectedRouteId = route.name === "session" ? route.id : null;
  useEffect(() => {
    if (!state.hydrated) return;
    let cancelled = false;
    void client.retainSelectedSession(selectedId).then((found) => {
      if (!cancelled && selectedId && !found && selectedRouteId === selectedId) navigate("/");
    });
    return () => { cancelled = true; };
  }, [client, selectedId, selectedRouteId, state.hydrated]);
  const selectedSession = selectedId ? state.sessions.get(selectedId) : undefined;
  const selectedAttentionUnseen =
    (selectedSession?.needsInputUnseen || selectedSession?.idleUnseen) ?? false;
  useEffect(() => {
    if (!selectedId || !selectedAttentionUnseen) return;
    void client.markSessionSeen(BigInt(selectedId)).catch((reason) => {
      console.error("failed to mark session attention seen", reason);
    });
  }, [client, selectedId, selectedAttentionUnseen]);
  const unread = needsInputCount(state.sessions.values());

  useEffect(() => {
    if (!focusMode) return;
    const exitFocusMode = (event: KeyboardEvent) => {
      const action = focusModeActionForKey(event.key);
      if (action) {
        event.preventDefault();
        event.stopPropagation();
        if (route.name === "session") {
          if (isReviewOrPlanTab) {
            navigate(sessionRoutePath(route.id, "agent", false));
          } else {
            navigate(sessionRoutePath(route.id, route.tab, false));
          }
        }
        updateFocusMode(action);
      }
    };
    // xterm consumes keyboard events before they bubble to window, so Focus
    // mode's documented Escape exit must observe the key in capture phase.
    window.addEventListener("keydown", exitFocusMode, true);
    return () => window.removeEventListener("keydown", exitFocusMode, true);
  }, [focusMode, isReviewOrPlanTab, route]);

  const toggleFocusMode = () => {
    if (route.name === "session") {
      if (isReviewOrPlanTab) {
        navigate(sessionRoutePath(route.id, "agent", false));
        updateFocusMode("exit");
        return;
      }
      const next = !focusMode;
      navigate(sessionRoutePath(route.id, route.tab, next));
      updateFocusMode(next ? "enter" : "exit");
    } else {
      updateFocusMode("toggle");
    }
    // Clicking the floating toggle moves browser focus away from xterm. Return
    // it after the focused layout settles so typing can continue immediately.
    requestAnimationFrame(() => {
      document.querySelector<HTMLElement>(
        '.main-pane .restorable-view:not(.is-route-background) .term-layer[style*="visible"] .xterm-helper-textarea, ' +
        ".main-pane .restorable-view:not(.is-route-background) .workspace-pane.is-focused .xterm-helper-textarea",
      )?.focus();
    });
  };

  useEffect(() => {
    removeLegacyNavigationPreferences();
    void listWorkspaces()
      .then((listing) => {
        setWorkspaces(listing.workspaces);
      })
      .catch((error: unknown) => setWorkspaceError(error instanceof Error ? error.message : String(error)));
  }, []);

  // Where you were inside each session, so returning to one — from the
  // Board, say — lands on the tab you left rather than the agent. The
  // URL stays authoritative: this is consulted only when navigating
  // somewhere that names no tab, never to override one that does.
  const lastTabBySession = useRef<Record<string, string>>({});
  useEffect(() => {
    if (route.name !== "session") return;
    // The agent tab is left out of the address, so recording only what
    // the URL names never learns that someone went back to it — and a
    // return would restore whatever tab they had left before that.
    lastTabBySession.current[route.id] = route.tab ?? "agent";
  }, [route]);

  /// Every path back into a session goes through here, so a return
  /// lands on the tab the reader left rather than the default.
  const navigateRestoring = useCallback((path: string) => {
    navigate(withRememberedTab(path, lastTabBySession.current));
  }, []);

  const select = useCallback((id: string) => {
    writeString(SELECTED_SESSION_KEY, id);
    setHomeSessionId(id);
    navigate(sessionRoutePath(id, lastTabBySession.current[id]));
  }, []);

  useLayoutEffect(() => {
    if (route.name !== "session") return;
    if (route.id === homeSessionId) return;
    writeString(SELECTED_SESSION_KEY, route.id);
    setHomeSessionId(route.id);
  }, [homeSessionId, route]);

  useEffect(() => {
    document.title = unread > 0 ? `(${unread}) ${APP_TITLE}` : APP_TITLE;
    return () => {
      document.title = APP_TITLE;
    };
  }, [unread]);

  // On load, land on the home pane rather than an ended session. Runs
  // once after hydration. The URL hash persists a session across
  // reloads, and a daemon restart marks live sessions failed, so a
  // reopened route may point at an ended or gone session; redirect
  // those home. Only a still-live saved session is restored.
  const restoredRef = useRef(false);
  useEffect(() => {
    if (restoredRef.current || !state.hydrated) return;
    restoredRef.current = true;
    if (route.name === "session") {
      return;
    }
    if (route.name !== "home") return;
    const saved = readString(SELECTED_SESSION_KEY);
    const session = saved ? state.sessions.get(saved) : undefined;
    if (session && !sessionEnded(session)) navigate(`/session/${saved}`);
  }, [route, state.hydrated, state.sessions]);

  const previousSessions = useRef<ReadonlyMap<string, Session> | null>(null);
  useEffect(() => {
    if (!state.hydrated) return;
    const previous = previousSessions.current;
    previousSessions.current = state.sessions;
    if (!previous) return;
    releaseEndedOrRemovedSessions(previous, state.sessions, stage);
    if (route.name !== "session") return;
    if (!selectedSessionBecameUnavailable(previous, state.sessions, route.id)) return;
    const workspaceIds = [...workspaces]
      .sort((a, b) => a.position - b.position || a.id - b.id)
      .map((workspace) => workspace.id);
    const fallback = sessionFallbackPath(state.sessions.values(), workspaceIds, route.id);
    const fallbackSession = fallback.match(/^\/session\/(\d+)$/)?.[1] ?? null;
    writeString(SELECTED_SESSION_KEY, fallbackSession);
    setHomeSessionId(fallbackSession);
    navigate(fallback);
  }, [route, stage, state.hydrated, state.sessions, workspaces]);

  const enabledRef = useRef(notifyEnabled);
  enabledRef.current = notifyEnabled;
  notifier.soundEnabled = notifySound;

  useEffect(() => {
    notifier.onActivate = (id) => select(id);
    notifier.onActivateApproval = (id) => navigate(approvalRoutePath(id));
  }, [notifier, select]);

  // The daemon decides what is worth an alert; this only renders it.
  useEffect(() => {
    return client.onAlert((session, kind) => {
      if (!enabledRef.current) return;
      const id = session.id.toString();
      if (kind === SessionAlertKind.NEEDS_INPUT) {
        const project = client.getState().projects.get(session.projectId.toString());
        const notification = needsInputNotification(session, project);
        notifier.fire(notification.title, notification.body, id);
        return;
      }
      const notification = sessionAlertNotification(session, kind);
      notifier.fire(notification.title, notification.body, id, "finished");
    });
  }, [client, notifier]);

  // Said twice on purpose. A desktop notification is what reaches somebody who
  // is not looking at the tab, and it can be denied or unsupported, so the
  // banner is what guarantees it is said at all. It stays until dismissed: a
  // notice that fades on its own is one the user can miss entirely, and not
  // being missable is the whole point of raising it.
  const [securityNotices, setSecurityNotices] = useState<SecurityNotice[]>([]);
  useEffect(() => {
    return client.onSecurityNotice((notice) => {
      setSecurityNotices((current) => [...current, notice]);
      notifier.fire(
        securityNoticeTitle(notice),
        `${notice.subject}: ${notice.detail}`,
        `security-${notice.kind}-${notice.subject}`,
      );
    });
  }, [client, notifier]);

  const requestNotifyPermission = useCallback(() => {
    void notifier.requestPermission().then((perm) => {
      setNotifyPermission(perm);
      if (perm === "granted") {
        setNotifyEnabled(true);
        writeBool(NOTIFY_ENABLED_KEY, true);
      }
    });
  }, [notifier]);

  // Ask once on first load; after that only the manual control prompts,
  // so a decline is never re-nagged.
  useEffect(() => {
    const asked = readBool(NOTIFY_ASKED_KEY, false);
    if (!shouldAutoRequestNotify(notifier.supported, asked, notifier.permission)) return;
    writeBool(NOTIFY_ASKED_KEY, true);
    requestNotifyPermission();
  }, [notifier, requestNotifyPermission]);

  const enableNotifications = requestNotifyPermission;

  const toggleSound = () => {
    setNotifySound((v) => {
      writeBool(NOTIFY_SOUND_KEY, !v);
      return !v;
    });
  };

  const toggleEnded = () => {
    setShowEnded((v) => {
      writeBool(SHOW_ENDED_KEY, !v);
      return !v;
    });
  };

  const notify: NotifyControls = {
    support: notifier.support,
    permission: notifyPermission,
    enabled: notifyEnabled,
    sound: notifySound,
    onEnable: enableNotifications,
    onToggleSound: toggleSound,
  };

  const handleLogout = () => {
    setAccountMenuOpen(false);
    void logout().finally(onLoggedOut);
  };

  const spawnFromItem = useCallback((item: Item) => {
    setSpawn({
      projectId: item.projectId?.toString(),
      lock: false,
      prefill: {
        title: item.title,
        prompt: itemSpawnPrompt(item),
        itemId: item.id,
        itemBucketId: item.bucketId,
      },
    });
  }, []);

  const routeItem = route.name === "item" ? route : null;
  const legacyRouteItemId = route.name === "legacy-item" ? route.legacyId : null;
  const snapshotRouteItem = routeItem === null ? undefined : state.items.get(itemKey(routeItem.bucketId, routeItem.id));
  useEffect(() => {
    if (legacyRouteItemId !== null) {
      setRoutedItem(undefined);
      setRoutedItemError("Legacy item links are unsupported because item numbers are bucket-scoped. Open the item from its bucket Board.");
      setRoutedItemLoading(false);
      return;
    }
    if (routeItem === null && legacyRouteItemId === null) {
      setRoutedItem(undefined);
      setRoutedItemError(null);
      setRoutedItemLoading(false);
      return;
    }
    if (snapshotRouteItem) {
      setRoutedItem(snapshotRouteItem);
      setRoutedItemError(null);
      setRoutedItemLoading(false);
      return;
    }
    const controller = new AbortController();
    setRoutedItem(undefined);
    setRoutedItemError(null);
    setRoutedItemLoading(true);
    fetchItem(routeItem!.bucketId, routeItem!.id, controller.signal)
      .then(setRoutedItem)
      .catch((error: unknown) => {
        if (!controller.signal.aborted) setRoutedItemError(error instanceof Error ? error.message : String(error));
      })
      .finally(() => { if (!controller.signal.aborted) setRoutedItemLoading(false); });
    return () => controller.abort();
  }, [routeItem, legacyRouteItemId, snapshotRouteItem]);

  const focusedItem = route.name === "item"
    ? state.items.get(itemKey(route.bucketId, route.id)) ?? routedItem
    : route.name === "legacy-item" ? routedItem : undefined;
  const boardBucketId =
    route.name === "board" || route.name === "item" ? route.bucketId : focusedItem?.bucketId.toString();
  const [mountedBoardBuckets, setMountedBoardBuckets] = useState<Set<string>>(() =>
    new Set(boardBucketId ? [boardBucketId] : []),
  );

  useEffect(() => {
    if (!boardBucketId) return;
    setMountedBoardBuckets((current) => {
      if (current.has(boardBucketId)) return current;
      const next = new Set(current);
      next.add(boardBucketId);
      return next;
    });
  }, [boardBucketId]);

  const boardInventory = useCallback((bucketId: string): BoardViewInventory => {
    const projectIds = new Set(
      [...state.projects.values()]
        .filter((project) => project.bucketId.toString() === bucketId)
        .map((project) => project.id.toString()),
    );
    return {
      liveSessionIds: new Set(
        [...state.sessions.values()]
          .filter((session) => projectIds.has(session.projectId.toString()) && !sessionEnded(session))
          .map((session) => session.id.toString()),
      ),
      workspaceIds: new Set(workspaces.map((workspace) => workspace.id)),
    };
  }, [state.projects, state.sessions, workspaces]);

  const rememberCurrentView = useCallback((bucketId: string) => {
    const view = restorableView(route);
    if (!view) return;
    const inventory = boardInventory(bucketId);
    const valid = view.name === "session"
      ? inventory.liveSessionIds.has(view.id)
      : inventory.workspaceIds.has(view.id);
    if (!valid) return;
    setBoardViews((current) => rememberBoardView(current, bucketId, view));
  }, [boardInventory, route]);

  // Session homes naturally identify their bucket, so visiting one refreshes
  // that bucket's return target even when Board is later opened by a PM link.
  useEffect(() => {
    if (route.name !== "session") return;
    const session = state.sessions.get(route.id);
    if (!session || sessionEnded(session)) return;
    const bucketId = state.projects.get(session.projectId.toString())?.bucketId.toString();
    if (!bucketId) return;
    setBoardViews((current) => rememberBoardView(current, bucketId, route));
  }, [route, state.projects, state.sessions]);

  const enterBoard = useCallback((bucketId: string, path = `/bucket/${bucketId}/board`) => {
    rememberCurrentView(bucketId);
    navigate(path);
  }, [rememberCurrentView]);

  const handlePmLink = useCallback(
    (link: PmLink, sourceSessionId?: string) => {
      if (link.kind === "legacyItem") {
        navigate(`/item/${link.legacyId}`);
        return;
      }
      if (link.kind === "item" && !client.getState().items.has(itemKey(link.bucketId, link.id))) {
        navigate(`/bucket/${link.bucketId}/item/${link.id}`);
        return;
      }
      const resolved = resolvePmLink(client.getState(), link, sourceSessionId);
      if (!resolved) return;
      switch (resolved.kind) {
        case "session":
          select(resolved.id);
          break;
        case "item": {
          const item = client.getState().items.get(itemKey(resolved.bucketId, resolved.id));
          if (item) enterBoard(item.bucketId.toString(), `/bucket/${resolved.bucketId}/item/${resolved.id}`);
          break;
        }
        case "legacyItem":
          navigate(`/item/${resolved.legacyId}`);
          break;
        case "bucket":
          enterBoard(resolved.id);
          break;
        case "project": {
          const project = client.getState().projects.get(resolved.id);
          if (project) enterBoard(project.bucketId.toString());
          break;
        }
        case "spawn":
          // Prefill only; the spawn button stays a human click.
          setSpawn({ projectId: resolved.projectId, lock: false, prefill: { prompt: resolved.prompt } });
          break;
      }
    },
    [client, enterBoard, select],
  );

  const openBoard = useCallback((bucketId: string) => {
    if (boardBucketId === bucketId) {
      navigateRestoring(boardReturnPath(boardViews, bucketId, boardInventory(bucketId)));
      return;
    }
    enterBoard(bucketId);
  }, [boardBucketId, boardInventory, boardViews, enterBoard]);

  const isBoardRoute = route.name === "board" || route.name === "item" || route.name === "legacy-item";
  const isSettingsRoute = route.name === "settings";
  const pendingApprovals = usePendingApprovals((approval) => {
    if (!enabledRef.current) return;
    notifier.fire(
      `${approval.project_name ?? "A session"}: connection approval requested`,
      `${approval.tool} on ${approval.connection_name ?? "a connection"}`,
      approval.id,
      "approval",
    );
  });

  const settingsInventory = useCallback((): SettingsReturnInventory => {
    const liveSessions = [...state.sessions.values()]
      .filter((session) => !sessionEnded(session))
      .sort((a, b) => Number(b.createdAtUnixMs - a.createdAtUnixMs));
    const savedWorkspaces = [...workspaces]
      .sort((a, b) => b.updatedAtUnixMs - a.updatedAtUnixMs);
    const retainedViews = [...workspaces]
      .sort((a, b) => a.position - b.position || a.id - b.id)
      .map((workspace) => ({ name: "workspace" as const, id: workspace.id }));
    return {
      liveSessionIds: new Set(liveSessions.map((session) => session.id.toString())),
      workspaceIds: new Set(savedWorkspaces.map((workspace) => workspace.id)),
      retainedViews,
      recentViews: [
        ...liveSessions.map((session) => ({
          view: { name: "session" as const, id: session.id.toString() },
          updatedAt: Number(session.createdAtUnixMs),
        })),
        ...savedWorkspaces.map((workspace) => ({
          view: { name: "workspace" as const, id: workspace.id },
          updatedAt: workspace.updatedAtUnixMs,
        })),
      ].sort((a, b) => b.updatedAt - a.updatedAt).map(({ view }) => view),
    };
  }, [state.sessions, workspaces]);

  const enterSettings = useCallback((
    section: SettingsSection,
    target?: { bucketId: string; projectId?: string },
  ) => {
    const liveSessionIds = new Set(
      [...state.sessions.values()]
        .filter((session) => !sessionEnded(session))
        .map((session) => session.id.toString()),
    );
    const returnTarget = settingsEntryReturnTarget(route, homeSessionId, liveSessionIds);
    const serialized = serializeSettingsReturnTarget(returnTarget);
    if (serialized) {
      writeSessionString(SETTINGS_RETURN_TARGET_KEY, serialized);
      setSettingsReturnTarget(parseSettingsReturnTarget(serialized));
    }
    navigate(settingsRoutePath(section, target));
  }, [homeSessionId, route, state.sessions]);

  const restoreFromSettings = useCallback(() => {
    navigateRestoring(settingsReturnPath(settingsReturnTarget, settingsInventory()));
  }, [settingsInventory, settingsReturnTarget]);

  const restoreFromBoard = useCallback(() => {
    if (!boardBucketId) {
      navigate("/");
      return;
    }
    navigateRestoring(boardReturnPath(boardViews, boardBucketId, boardInventory(boardBucketId)));
  }, [boardBucketId, boardInventory, boardViews]);

  const newProjectInBucket = useCallback((bucketId: string) => {
    setSettingsBucket(bucketId);
    enterSettings("projects");
  }, [enterSettings]);
  const newSessionInBucket = useCallback((bucketId: string) => {
    setSpawn({ bucketId, lock: false });
  }, []);
  const openBucketSettings = useCallback((bucketId: string) => {
    enterSettings("projects", { bucketId });
  }, [enterSettings]);
  const clearSettingsBucket = useCallback(() => setSettingsBucket(null), []);

  const saveWorkspace = useCallback((updated: SavedWorkspace) => {
    setWorkspaces((current) => current.map((workspace) => workspace.id === updated.id ? updated : workspace));
  }, []);

  const previousTerminals = useRef<ReadonlyMap<string, Terminal> | null>(null);
  useEffect(() => {
    if (!state.hydrated) return;
    const previous = previousTerminals.current;
    previousTerminals.current = state.terminals;
    if (!previous) return;
    releaseRemovedTerminals(previous, state.terminals, stage);
    const removedShellIds = [...previous]
      .filter(([id, terminal]) => terminal.kind === TerminalKind.SHELL && !state.terminals.has(id))
      .map(([id]) => id);
    if (removedShellIds.length === 0) return;

    for (const id of removedShellIds) stage.disposeTerminal(BigInt(id));
    setWorkspaces((current) => current.map((workspace) => {
      const parsed = parseWorkspaceLayout(workspace.layout);
      if (!parsed) return workspace;
      const layout = removedShellIds.reduce(removeTerminalFromLayout, parsed);
      if (layout === parsed) return workspace;
      const updated = { ...workspace, layout };
      void updateWorkspace(updated)
        .then(saveWorkspace)
        .catch((error: unknown) => setWorkspaceError(error instanceof Error ? error.message : String(error)));
      return updated;
    }));
  }, [saveWorkspace, stage, state.hydrated, state.terminals]);

  const reorderWorkspaceTabs = useCallback((workspaceIds: number[]) => {
    setWorkspaces((current) => {
      const byId = new Map(current.map((workspace) => [workspace.id, workspace]));
      return workspaceIds.flatMap((id, position) => {
        const workspace = byId.get(id);
        return workspace ? [{ ...workspace, position }] : [];
      });
    });
    void reorderWorkspaces(workspaceIds).catch((error: unknown) => {
      setWorkspaceError(error instanceof Error ? error.message : String(error));
      void listWorkspaces().then((listing) => {
        setWorkspaces(listing.workspaces);
      });
    });
  }, []);

  const confirmWorkspaceDelete = useCallback((): Promise<void> => {
    if (!workspaceDelete) return Promise.resolve();
    return deleteWorkspace(workspaceDelete.id).then(() => {
      const next = removeWorkspaceFromTabs(workspaces, workspaceDelete.id);
      setWorkspaces(next);
      if (route.name === "workspace" && route.id === workspaceDelete.id) {
        navigate(homeSessionId ? `/session/${homeSessionId}` : "/");
      }
      setWorkspaceDelete(null);
    });
  }, [homeSessionId, route, workspaceDelete, workspaces]);

  const newWorkspace = useCallback(() => setWorkspaceDialog({ kind: "create" }), []);

  const submitWorkspaceName = useCallback((name: string): Promise<void> => {
    if (workspaceDialog?.kind === "rename") {
      return updateWorkspace({ ...workspaceDialog.workspace, name }).then((workspace) => {
        saveWorkspace(workspace);
        setWorkspaceDialog(null);
      });
    }
    const sessionId = sessionHomeId(route, homeSessionId);
    const terminal = sessionId ? agentTerminalForSession(state.terminals.values(), sessionId) : undefined;
    const layout = { kind: "pane" as const, paneId: crypto.randomUUID(), terminalId: terminal?.id.toString() ?? null };
    return createWorkspace(name, layout).then((workspace) => {
        setWorkspaces((current) => [...current, workspace]);
        setWorkspaceDialog(null);
        navigate(`/workspace/${workspace.id}`);
      });
  }, [homeSessionId, route, saveWorkspace, state.terminals, workspaceDialog]);

  const activeView = restorableView(route);
  const retainedBoardView = boardBucketId
    ? validBoardView(boardViews, boardBucketId, boardInventory(boardBucketId))
    : null;
  const retainedSettingsView = isSettingsRoute
    ? settingsReturnView(settingsReturnTarget, settingsInventory())
    : null;
  const displayedView = activeView ?? retainedBoardView ?? retainedSettingsView;
  const selectedWorkspace = displayedView?.name === "workspace"
    ? workspaces.find((workspace) => workspace.id === displayedView.id)
    : undefined;

  // The tallies count what the sidebar can list, so its numbers and the rows
  // it filters to always describe the same set.
  const talliedSessions = visibleSessions(state.sessions.values(), showEnded, selectedId);
  // A filter whose last session moves on is dropped rather than left showing nothing.
  const activeStateFilter = stateFilter && rollupCounts(talliedSessions)[stateFilter] > 0
    ? stateFilter
    : null;

  const addSessionToWorkspace = useCallback((sessionId: string) => {
    if (route.name !== "workspace") return false;
    const terminal = agentTerminalForSession(state.terminals.values(), sessionId);
    if (!terminal) return false;
    setWorkspaceTerminal(terminal.id.toString());
    return true;
  }, [route, state.terminals]);

  return (
    <div className={`shell shell-fullbleed ${focusMode ? "is-focus-mode" : ""} ${isBoardRoute ? "is-board-route" : ""} ${isSettingsRoute ? "is-settings-route" : ""}`}>
      <header className="topbar">
        <a
          href="#/"
          className="topbar-brand"
          aria-label={isSettingsRoute ? "Home, return to previous view" : "Home"}
          onClick={(e) => {
            e.preventDefault();
            if (isSettingsRoute) restoreFromSettings();
            else if (isBoardRoute) restoreFromBoard();
            else navigate("/");
          }}
        >
          <ProductBrand variant="mark" decorative />
        </a>
        <BuildStamp />
        <ConnStatus phase={state.conn} />
        <StatusTallies
          sessions={talliedSessions}
          active={activeStateFilter}
          onPick={(picked) => setStateFilter((current) => (current === picked ? null : picked))}
        />
        <div className="topbar-spacer" />
        <AppearanceToggle appearance={appearance} />
        <ApprovalsButton pending={pendingApprovals} />
        <button
          type="button"
          className="btn topbar-icon focus-mode-toggle"
          title={focusMode ? "exit focus mode (Escape)" : "enter focus mode"}
          aria-label={focusMode ? "exit focus mode" : "enter focus mode"}
          aria-pressed={focusMode}
          onClick={toggleFocusMode}
        >
          <svg viewBox="0 0 16 16" aria-hidden="true">
            <path d="M6 2H2v4M10 2h4v4M14 10v4h-4M6 14H2v-4" />
          </svg>
        </button>
        <a
          className="topbar-icon settings-link"
          href={`#${settingsRoutePath("appearance")}`}
          title="Settings"
          aria-label="Settings"
          aria-current={isSettingsRoute ? "page" : undefined}
          onClick={(event) => {
            event.preventDefault();
            if (!isSettingsRoute) enterSettings("appearance");
          }}
        >
          <svg viewBox="0 0 24 24" aria-hidden="true">
            <path d="M12.22 2h-.44a2 2 0 0 0-2 2v.18a2 2 0 0 1-1 1.73l-.43.25a2 2 0 0 1-2 0l-.15-.08a2 2 0 0 0-2.73.73l-.22.38a2 2 0 0 0 .73 2.73l.15.1a2 2 0 0 1 1 1.72v.51a2 2 0 0 1-1 1.74l-.15.09a2 2 0 0 0-.73 2.73l.22.38a2 2 0 0 0 2.73.73l.15-.08a2 2 0 0 1 2 0l.43.25a2 2 0 0 1 1 1.73V20a2 2 0 0 0 2 2h.44a2 2 0 0 0 2-2v-.18a2 2 0 0 1 1-1.73l.43-.25a2 2 0 0 1 2 0l.15.08a2 2 0 0 0 2.73-.73l.22-.39a2 2 0 0 0-.73-2.73l-.15-.08a2 2 0 0 1-1-1.74v-.5a2 2 0 0 1 1-1.74l.15-.09a2 2 0 0 0 .73-2.73l-.22-.38a2 2 0 0 0-2.73-.73l-.15.08a2 2 0 0 1-2 0l-.43-.25a2 2 0 0 1-1-1.73V4a2 2 0 0 0-2-2z" />
            <circle cx="12" cy="12" r="3" />
          </svg>
        </a>
        <Popover
          open={accountMenuOpen}
          onToggle={() => setAccountMenuOpen((open) => !open)}
          onClose={() => setAccountMenuOpen(false)}
          triggerLabel={username}
          triggerTitle={`Account menu for ${username}`}
          triggerClassName="topbar-user account-menu-trigger"
        >
          <div className="account-menu-identity" aria-hidden="true">signed in as <strong>{username}</strong></div>
          <button type="button" role="menuitem" className="popover-item" onClick={handleLogout}>
            Log out
          </button>
        </Popover>
      </header>
      {securityNotices.map((notice, index) => (
        <div className="flash-security" role="alert" key={`${notice.kind}-${notice.subject}-${index}`}>
          <span>
            <strong>{securityNoticeTitle(notice)}</strong> — {notice.subject}: {notice.detail}
          </span>
          <button
            type="button"
            className="btn"
            onClick={() => setSecurityNotices((current) => current.filter((_, i) => i !== index))}
          >
            dismiss
          </button>
        </div>
      ))}
      <div
        className="workspace"
        style={{ "--sidebar-w": `${sidebarWidth}px` } as CSSProperties}
      >
        <div className="sidebar-shell">
          <Sidebar
            selectedId={selectedId}
            onSelect={select}
            onAddToWorkspace={addSessionToWorkspace}
            onNewProject={newProjectInBucket}
            onNewSession={newSessionInBucket}
            onOpenBucketSettings={openBucketSettings}
            onOpenBoard={openBoard}
            onPmLink={handlePmLink}
            activeBoardBucketId={boardBucketId}
            showEnded={showEnded}
            onToggleEnded={toggleEnded}
            stateFilter={activeStateFilter}
            onClearStateFilter={() => setStateFilter(null)}
            notify={notify}
          />
        </div>
        <div
          className={`sidebar-resizer ${resizing ? "is-dragging" : ""}`}
          role="separator"
          aria-orientation="vertical"
          aria-label="resize sidebar"
          onPointerDown={startResize}
        />
        <main className="main-pane">
          {displayedView && (
            <div
              className={`restorable-view ${activeView ? "" : `is-route-background ${isBoardRoute ? "is-board-background" : ""}`}`}
              aria-hidden={!activeView}
              // The retained view stays mounted to preserve terminal buffers,
              // viewport, tab selection, and pane focus, but cannot receive
              // pointer or keyboard input behind a full-screen route.
              inert={!activeView ? true : undefined}
            >
              <ErrorBoundary key={displayedView.name} resetKey={displayedView.id}>
                <WorkspaceTabs route={displayedView} workspaces={workspaces} onCreate={newWorkspace} onDelete={setWorkspaceDelete} onRename={(workspace) => setWorkspaceDialog({ kind: "rename", workspace })} onReorder={reorderWorkspaceTabs} />
                {workspaceError && <div className="flash-error workspace-flash" role="alert">{workspaceError}</div>}
                {displayedView.name === "session" && (
                  <SessionPane
                    sessionId={displayedView.id}
                    stage={stage}
                    onSelect={select}
                    active={Boolean(activeView)}
                    routeTab={route.name === "session" && route.id === displayedView.id ? route.tab : undefined}
                    routeFocus={focusMode}
                    onReviewFinished={reviewFinished}
                  />
                )}
                {displayedView.name === "workspace" && selectedWorkspace && (
                  <WorkspacePage
                    workspace={selectedWorkspace}
                    stage={stage}
                    onUpdated={saveWorkspace}
                    addTerminalId={workspaceTerminal}
                    onTerminalAdded={() => setWorkspaceTerminal(undefined)}
                    active={Boolean(activeView)}
                  />
                )}
                {displayedView.name === "workspace" && state.hydrated && !selectedWorkspace && (
                  <div className="pane-empty"><p className="muted-line">this workspace no longer exists</p></div>
                )}
              </ErrorBoundary>
            </div>
          )}
          {!activeView && workspaceError && <div className="flash-error workspace-flash" role="alert">{workspaceError}</div>}
          <ErrorBoundary key={`${route.name}:${"id" in route ? route.id : "bucketId" in route ? route.bucketId : ""}`}>
          {route.name === "home" && (
            <WorkspaceTabs route={route} workspaces={workspaces} onCreate={newWorkspace} onDelete={setWorkspaceDelete} onRename={(workspace) => setWorkspaceDialog({ kind: "rename", workspace })} onReorder={reorderWorkspaceTabs} />
          )}
          {route.name === "settings" && (
            <SettingsPage
              username={username}
              section={route.section}
              catalog={route.catalog}
              onBack={restoreFromSettings}
              preselectBucket={settingsBucket}
              onPreselectConsumed={clearSettingsBucket}
              selectedBucketId={route.bucketId}
              selectedProjectId={route.projectId}
              connection={route.connection}
            />
          )}
          {route.name === "approvals" && <ApprovalsPage id={route.id} />}
          {route.name === "review" && (
            <ReviewPage
              id={route.id}
              at={{ view: route.view, file: route.file, thread: route.thread }}
              onFinished={reviewFinished}
            />
          )}
          {(route.name === "item" || route.name === "legacy-item") && state.hydrated && routedItemLoading && (
            <div className="pane-empty"><p className="muted-line">loading item…</p></div>
          )}
          {(route.name === "item" || route.name === "legacy-item") && state.hydrated && !routedItemLoading && !focusedItem && (
            <div className="pane-empty">
              <p className="muted-line">{routedItemError ?? "this item no longer exists"}</p>
            </div>
          )}
          {route.name === "home" && <HomePane />}
          </ErrorBoundary>
          {[...mountedBoardBuckets].map((bucketId) => {
            const active = isBoardRoute && boardBucketId === bucketId;
            return (
              <div
                key={bucketId}
                className={`board-route-view ${active ? "is-active" : ""}`}
                aria-hidden={!active}
                inert={!active ? true : undefined}
              >
                <ErrorBoundary>
                  <BoardPage
                    bucketId={bucketId}
                    active={active}
                    focusItemId={active && route.name === "item" ? route.id : undefined}
                    focusItem={active ? focusedItem : undefined}
                    onPmLink={handlePmLink}
                    onSpawnFromItem={spawnFromItem}
                    planHref={(sessionId, planId) => sessionTabHref(location, sessionId, planTabId(planId))}
                  />
                </ErrorBoundary>
              </div>
            );
          })}
        </main>
        <ConnectionSessionPanel sessionId={selectedRouteId} />
      </div>
      {spawn && (
        <SpawnDialog
          onClose={() => setSpawn(null)}
          onCreated={select}
          defaultProjectId={spawn.projectId}
          bucketId={spawn.bucketId}
          lockProject={spawn.lock}
          prefill={spawn.prefill}
        />
      )}
      {workspaceDialog && (
        <WorkspaceDialog
          title={workspaceDialog.kind === "create" ? "new workspace" : "rename workspace"}
          initialName={workspaceDialog.kind === "create" ? `workspace ${workspaces.length + 1}` : workspaceDialog.workspace.name}
          submitLabel={workspaceDialog.kind === "create" ? "create workspace" : "rename"}
          onClose={() => setWorkspaceDialog(null)}
          onSubmit={submitWorkspaceName}
        />
      )}
      {workspaceDelete && (
        <WorkspaceDeleteDialog
          workspaceName={workspaceDelete.name}
          onClose={() => setWorkspaceDelete(null)}
          onConfirm={confirmWorkspaceDelete}
        />
      )}
    </div>
  );
}

function HomePane() {
  const state = useAppState();
  if (!state.hydrated) {
    return (
      <div className="pane-empty">
        <p className="muted-line">
          {state.conn === "offline" ? "daemon unreachable — retrying…" : "connecting…"}
        </p>
      </div>
    );
  }
  return (
    <div className="pane-empty">
      <p className="pane-empty-title">select a session</p>
      <p className="muted-line">pick one from the sidebar, or spawn from a project</p>
    </div>
  );
}

function ConnStatus({ phase }: { phase: "connecting" | "online" | "offline" }) {
  return (
    <span className={`conn conn-${phase}`}>
      <span className="conn-dot" aria-hidden="true" />
      <span className="sentence">{phase}</span>
    </span>
  );
}
