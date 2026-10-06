import { StatusBar } from "expo-status-bar";
import * as Notifications from "expo-notifications";
import * as SecureStore from "expo-secure-store";
import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import { Alert, AppState, SafeAreaView, StyleSheet, Text } from "react-native";
import { KeyboardAvoidingRoot } from "./src/components/KeyboardAvoidingRoot";
import { PrivacyScreen } from "./src/components/PrivacyScreen";

import { PmClient } from "@puppet-master/client-core/ws/client";
import { PLAN_FOCUS_KEY } from "@puppet-master/client-core/state/planFocus";

import { bindClientToForeground, isForeground, watchForeground } from "./src/adapters/lifecycle";
import { createSocketConnector, reactNativeWebSocketFactory } from "./src/adapters/socket";
import { CachedSecureStorage } from "./src/adapters/storage";
import { ALL_AUTH_KEYS, AuthRejectedError, DeviceAuthSession, type DeviceAuthStatus } from "./src/auth/session";
import {
  configureForegroundNotifications,
  listenForTokenChanges,
  requestAndRegisterPush,
  deviceLog,
} from "./src/push";
import {
  ALL_CONFIG_KEYS,
  readControllerConfig,
  writeControllerConfig,
  type ControllerConfig,
} from "./src/config";
import {
  describePayload,
  extractPayload,
  launchTapDecision,
  tapDecision,
  LAST_ROUTED_NOTIFICATION_KEY,
  type NotificationAction,
  type ResponsePayload,
} from "./src/pushRoute";
import { LoginScreen } from "./src/screens/LoginScreen";
import { SettingsScreen } from "./src/screens/SettingsScreen";
import { ApprovalsScreen } from "./src/screens/ApprovalsScreen";
import { usePendingApprovalCount } from "./src/api/pendingApprovals";
import { SessionsScreen, type TerminalTarget } from "./src/screens/SessionsScreen";
import { TerminalScreen } from "./src/screens/TerminalScreen";
import { PlanScreen, type PlanTarget } from "./src/screens/PlanScreen";
import { findActivePlanForSession } from "./src/planning/planRoute";
import { connectionBanner } from "./src/status";
import { readDevEnrollEnv, devAutoEnroll } from "./src/devAutoEnroll";

type Screen =
  | { kind: "login" }
  | { kind: "settings" }
  | { kind: "sessions" }
  | { kind: "terminal"; target: TerminalTarget }
  | { kind: "plan"; target: PlanTarget; returnTo: TerminalTarget | null }
  | { kind: "approvals"; approvalId: string | null };

const UNAUTH_RETRY_DELAY_MS = 5_000;

const UNENROLLED: DeviceAuthStatus = { kind: "unenrolled" };

export default function App() {
  const [storage, setStorage] = useState<CachedSecureStorage | null>(null);
  const [config, setConfig] = useState<ControllerConfig | null>(null);
  const [screen, setScreen] = useState<Screen>({ kind: "login" });
  const [legacyRejected, setLegacyRejected] = useState(false);
  const configRef = useRef<ControllerConfig | null>(null);
  configRef.current = config;

  useEffect(() => {
    let cancelled = false;
    void CachedSecureStorage.hydrate(
      SecureStore,
      [...ALL_CONFIG_KEYS, ...ALL_AUTH_KEYS, LAST_ROUTED_NOTIFICATION_KEY, PLAN_FOCUS_KEY],
      (key, err) => console.warn(`secure storage read failed for ${key}`, err),
    ).then((hydrated) => {
      if (cancelled) return;
      setStorage(hydrated);
      const saved = readControllerConfig(hydrated);
      const enrollEnv = readDevEnrollEnv();
      // In UI-test builds where the controller URL matches what's
      // persisted, load the cached config immediately so the WebSocket
      // connects without waiting for the enrollment effect. When the
      // URLs differ (new fixture), skip the cached config and let
      // enrollment set the correct one.
      if (enrollEnv && saved && saved.baseUrl !== enrollEnv.controllerUrl) {
        // Stale config from a different fixture — enrollment will replace it.
      } else if (saved) {
        setConfig(saved);
        setScreen({ kind: "sessions" });
      }
    });
    return () => {
      cancelled = true;
    };
  }, []);

  const auth = useMemo(
    () => (storage ? new DeviceAuthSession(storage, fetch) : null),
    [storage],
  );

  // UI-test auto-enrollment: when the manifest carries PM_UI_TEST_BUILD
  // and a controller URL (set by PM_UI_TEST_BUILD=1 at build time),
  // enroll without user interaction. The flag is absent in normal
  // development, ios-device, TestFlight, and release builds.
  useEffect(() => {
    if (!storage || !auth) return;
    const env = readDevEnrollEnv();
    if (!env) return;
    void devAutoEnroll(env, auth, storage).then((ok) => {
      if (ok) {
        setConfig({
          baseUrl: env.controllerUrl,
          sessionToken: null,
          pushPreviewsEnabled: false,
        });
        setScreen({ kind: "sessions" });
      }
    });
  }, [storage, auth]);
  const authRef = useRef<DeviceAuthSession | null>(null);
  authRef.current = auth;

  const client = useMemo(
    () =>
      new PmClient(
        createSocketConnector(
          () => ({
            baseUrl: configRef.current?.baseUrl ?? "http://unconfigured.invalid",
            accessToken: authRef.current?.accessToken(),
            sessionToken: configRef.current?.sessionToken,
          }),
          reactNativeWebSocketFactory,
        ),
      ),
    [],
  );

  useEffect(() => {
    if (!auth) return;
    let cancelled = false;
    let retryTimer: ReturnType<typeof setTimeout> | null = null;
    const recover = async () => {
      const baseUrl = configRef.current?.baseUrl;
      if (!baseUrl || cancelled) return;
      if (auth.status().kind !== "enrolled") {
        setLegacyRejected(true);
        return;
      }
      try {
        await auth.refresh(baseUrl);
        if (!cancelled) {
          client.stop();
          // A refresh can outlive the foreground it started in. Starting the
          // client here anyway would open a socket the app is not allowed to
          // keep, and leave the foreground edge with nothing left to do.
          if (isForeground(AppState.currentState)) client.start();
        }
      } catch (err) {
        if (err instanceof AuthRejectedError) return;
        if (!cancelled) {
          retryTimer = setTimeout(() => void recover(), UNAUTH_RETRY_DELAY_MS);
        }
      }
    };
    client.onUnauthenticated = () => void recover();
    return () => {
      cancelled = true;
      client.onUnauthenticated = null;
      if (retryTimer !== null) clearTimeout(retryTimer);
    };
  }, [auth, client]);

  useEffect(() => {
    if (!config) return;
    const unbind = bindClientToForeground(client, AppState);
    return unbind;
  }, [client, config]);

  // Proactive token refresh: keep the access token fresh while the app is
  // foregrounded so a terminal open never waits on a refresh round trip.
  useEffect(() => {
    if (!auth || !config) return;
    auth.startProactiveRefresh(config.baseUrl);
    const unbind = watchForeground(AppState, {
      onForeground: () => auth.startProactiveRefresh(config.baseUrl),
      onBackground: () => auth.stopProactiveRefresh(),
    });
    return () => {
      unbind();
      auth.stopProactiveRefresh();
    };
  }, [auth, config]);

  const authStatus = useSyncExternalStore(
    auth?.subscribe ?? (() => () => {}),
    auth?.status ?? (() => UNENROLLED),
  );
  const connPhase = useSyncExternalStore(client.subscribe, () => client.getState().conn);

  // Register for push notifications once the device is enrolled and
  // connected to a controller. The prompt appears after the sessions
  // screen, not on first launch. Token rotation is handled for the
  // lifetime of the enrollment.
  useEffect(() => {
    configureForegroundNotifications();
  }, []);

  useEffect(() => {
    if (!auth || !config || authStatus.kind !== "enrolled") {
      console.log(
        `[push] not registering: auth ${auth ? "ready" : "missing"}, ` +
          `controller ${config ? "configured" : "unset"}, ` +
          `enrollment ${authStatus.kind}`,
      );
      return;
    }
    // UI-test builds skip push registration so the system notification
    // permission dialog does not block automation.
    if (readDevEnrollEnv()) {
      console.log("[push] skipped: ui-test auto-enroll active");
      return;
    }
    const baseUrl = config.baseUrl;
    const previews = config.pushPreviewsEnabled;
    void requestAndRegisterPush(auth, baseUrl, fetch, previews);
    const unsubscribe = listenForTokenChanges(auth, baseUrl, fetch, previews);
    return unsubscribe;
  }, [auth, config, authStatus.kind]);
  const openPlan = useCallback((planTarget: PlanTarget, returnTo: TerminalTarget | null) => {
    setScreen({ kind: "plan", target: planTarget, returnTo });
  }, []);

  const applyAction = useCallback(
    (action: NotificationAction) => {
      if (action.kind === "navigate") {
        const state = client.getState();
        const planTarget = findActivePlanForSession(action.target.sessionId, state.plans);
        if (planTarget) {
          openPlan(
            { ...planTarget, sessionId: action.target.sessionId },
            action.target,
          );
          return;
        }
        setScreen({ kind: "terminal", target: action.target });
      } else if (action.kind === "approval") {
        setScreen({ kind: "approvals", approvalId: action.approvalId });
      } else if (action.kind === "alert") {
        Alert.alert(action.title, action.message);
      }
    },
    [client, openPlan],
  );

  const toPayload = (response: Notifications.NotificationResponse) =>
    extractPayload(
      response.notification.request.content.data,
      response.notification.request.trigger,
      response.notification.request.identifier,
    );

  // A tap is decided against live state. One that arrives before the
  // first snapshot is held and decided again when the snapshot lands,
  // whether the tap launched the app or the app was still connecting.
  const heldTap = useRef<ResponsePayload | null>(null);
  const routeTap = useCallback(
    (payload: ResponsePayload, origin: string) => {
      const state = client.getState();
      const decision = tapDecision(state.hydrated, payload, state.sessions, state.terminals);
      deviceLog(
        `[push] ${origin} tap ${decision.kind}: ${describePayload(payload)} ` +
          `hydrated=${state.hydrated} sessions=${state.sessions.size} ` +
          `terminals=${state.terminals.size}`,
      );
      if (decision.kind === "hold") {
        heldTap.current = payload;
        return;
      }
      heldTap.current = null;
      storage?.setItem(LAST_ROUTED_NOTIFICATION_KEY, payload.identifier);
      Notifications.clearLastNotificationResponse();
      applyAction(decision);
    },
    [client, storage, applyAction],
  );

  useEffect(() => {
    deviceLog("[push] response listener attached");
    const sub = Notifications.addNotificationResponseReceivedListener((response) => {
      deviceLog("[push] response listener fired");
      routeTap(toPayload(response), "warm");
    });
    return () => {
      deviceLog("[push] response listener detached");
      sub.remove();
    };
  }, [routeTap]);

  // The tap that launched the app is delivered before any listener
  // exists, so it is read back once storage can say whether an earlier
  // process already routed it.
  const launchTapChecked = useRef(false);
  useEffect(() => {
    if (!storage || launchTapChecked.current) return;
    launchTapChecked.current = true;
    const response = Notifications.getLastNotificationResponse();
    const payload = response ? toPayload(response) : null;
    const decision = launchTapDecision(payload, storage.getItem(LAST_ROUTED_NOTIFICATION_KEY));
    if (decision !== "route" || !payload) {
      deviceLog(`[push] launch tap ${decision}: response=${response ? "present" : "none"}`);
      return;
    }
    routeTap(payload, "launch");
  }, [storage, routeTap]);

  const hydrated = useSyncExternalStore(client.subscribe, () => client.getState().hydrated);
  useEffect(() => {
    if (!hydrated) return;
    if (!heldTap.current) {
      deviceLog("[push] hydrated with no held tap");
      return;
    }
    routeTap(heldTap.current, "held");
  }, [hydrated, routeTap]);

  // The menu badge only matters on the sessions list, so it is polled there.
  const pendingApprovals = usePendingApprovalCount(
    auth,
    config?.baseUrl ?? null,
    screen.kind === "sessions" && authStatus.kind === "enrolled",
  );

  const banner = connectionBanner(connPhase, authStatus, {
    hasLegacySession: Boolean(config?.sessionToken),
    legacyRejected,
  });

  // Persisting the controller config and leaving the screen you are on are
  // separate things. Enrolling ends the login screen, so it does both;
  // changing a setting does not, or flipping a switch throws you out of
  // settings mid-change.
  const saveConfig = (next: ControllerConfig) => {
    setConfig(next);
    setLegacyRejected(false);
    if (storage) writeControllerConfig(storage, next);
  };

  const enrol = (next: ControllerConfig) => {
    saveConfig(next);
    setScreen({ kind: "sessions" });
  };

  const handleLogOut = () => {
    if (!auth) return;
    auth.signOut();
    client.stop();
    setConfig(null);
    setLegacyRejected(false);
    setScreen({ kind: "login" });
  };

  // TerminalScreen manages its own keyboard geometry for PTY resizing, so
  // the root-level keyboard avoidance is disabled when the terminal is active.
  const terminalActive = screen.kind === "terminal";

  return (
    <SafeAreaView style={styles.root}>
      <StatusBar style="light" />
      <PrivacyScreen />
      <KeyboardAvoidingRoot enabled={!terminalActive}>
      {!storage || !auth ? (
        <Text style={styles.loading}>Loading…</Text>
      ) : screen.kind === "login" ? (
        <LoginScreen
          initial={config}
          auth={auth}
          notice={banner.needsEnrollment ? banner.detail : null}
          onEnrolled={enrol}
        />
      ) : screen.kind === "settings" && config ? (
        <SettingsScreen
          config={config}
          auth={auth}
          banner={banner}
          onSave={saveConfig}
          onLogOut={handleLogOut}
          onBack={() => setScreen({ kind: "sessions" })}
        />
      ) : screen.kind === "sessions" ? (
        <SessionsScreen
          client={client}
          storage={storage}
          banner={banner}
          onOpenTerminal={(target) => setScreen({ kind: "terminal", target })}
          onOpenSettings={() => setScreen({ kind: "settings" })}
          onOpenApprovals={() => setScreen({ kind: "approvals", approvalId: null })}
          pendingApprovals={pendingApprovals}
        />
      ) : screen.kind === "approvals" && config ? (
        <ApprovalsScreen
          config={config}
          auth={auth}
          approvalId={screen.approvalId}
          onSelect={(approvalId) => setScreen({ kind: "approvals", approvalId })}
          onBack={() => setScreen({ kind: "sessions" })}
        />
      ) : screen.kind === "terminal" && config ? (
        <TerminalScreen
          config={config}
          target={screen.target}
          client={client}
          auth={auth}
          onBack={() => setScreen({ kind: "sessions" })}
          onSwitchTerminal={(target) => setScreen({ kind: "terminal", target })}
          onOpenPlan={(planTarget) => openPlan(planTarget, screen.target)}
        />
      ) : screen.kind === "plan" && config ? (
        <PlanScreen
          config={config}
          target={screen.target}
          client={client}
          auth={auth}
          storage={storage}
          onBack={() => {
            if (screen.returnTo) {
              setScreen({ kind: "terminal", target: screen.returnTo });
            } else {
              setScreen({ kind: "sessions" });
            }
          }}
        />
      ) : null}
      </KeyboardAvoidingRoot>
    </SafeAreaView>
  );
}

const styles = StyleSheet.create({
  root: { flex: 1, backgroundColor: "#101014" },
  loading: { color: "#9aa4b6", textAlign: "center", marginTop: 96 },
});
