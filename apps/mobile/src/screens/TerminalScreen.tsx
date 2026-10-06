import { useCallback, useEffect, useMemo, useRef, useState, useSyncExternalStore } from "react";
import {
  Alert,
  Animated,
  AppState,
  Clipboard,
  Dimensions,
  Easing,
  Keyboard,
  Linking,
  PanResponder,
  Platform,
  Pressable,
  ScrollView,
  StyleSheet,
  Text,
  View,
  type GestureResponderEvent,
  type KeyboardEvent,
  type LayoutChangeEvent,
} from "react-native";
import { WebView, type WebViewMessageEvent } from "react-native-webview";

import type { PmClient } from "@puppet-master/client-core/ws/client";
import type { Plan, SessionForward, Terminal } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { PlanState, TerminalKind } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { openableForwardUrl } from "@puppet-master/client-core/api/forwards";
import { forwardsForSession } from "@puppet-master/client-core/state/forwards";

import { websocketBaseUrl } from "../adapters/socket";
import { bearerJsonFetch, mintTerminalAttachTicket } from "../auth/api";
import type { DeviceAuthSession } from "../auth/session";
import type { ControllerConfig } from "../config";
import { colors } from "../theme";
import { bytesToBase64 } from "../terminal/base64";
import { AccessoryRowVisibility } from "../terminal/accessoryVisibility";
import { bindTerminalVisibility } from "../terminal/visibility";
import { TerminalAttachController } from "../terminal/attach";
import { shouldBearerReInit } from "../terminal/bearerReconnect";
import { createReplayVeil } from "../terminal/replayVeil";
import { createStatusGrace } from "../terminal/statusGrace";
import { KeyboardShiftPlanner, type KeyboardShiftDirective } from "../terminal/keyboardShift";
import { TerminalPanBridge, type PanSample } from "../terminal/panBridge";
import { terminalHtml } from "../terminal/gen/terminalHtml";
import { SwiftTermSurface } from "../terminal/SwiftTermSurface";
import type { SwiftTermSurfaceHandle } from "../terminal/SwiftTermSurface.types";
import {
  TERMINAL_REPLAY_CAP_BYTES,
  encodeHostMessage,
  parseViewMessage,
  type HostMessage,
  type ViewMessage,
} from "../terminal/protocol";
import type { PanPhase } from "../terminal/touchScroll";
import type { TerminalTarget } from "./SessionsScreen";
import type { PlanTarget } from "./PlanScreen";
import { OverflowButton, OverflowMenu, type OverflowMenuItem } from "./OverflowMenu";
import { SessionSheet } from "./SessionSheet";
import { TerminalsSheet } from "./TerminalsSheet";

const TERMINAL_FONT_SIZE = 13;

// Bracketed-paste escape sequences: when the foreground application has
// negotiated bracketed paste mode, pasted text must be wrapped so the
// shell treats it as a single paste rather than executing each line.
const BRACKETED_PASTE_START = "\x1b[200~";
const BRACKETED_PASTE_END = "\x1b[201~";

// Approximates UIKit's keyboard animation curve; the keyboard event carries
// a duration but no curve Animated can consume directly.
const KEYBOARD_EASING = Easing.bezier(0.38, 0.7, 0.125, 1);

// ---------------------------------------------------------------------------
// Sticky modifier state: off → armed (one-shot) → locked → off.
// Tap once to arm; the next non-modifier key sends the modified sequence
// and disarms. Tap twice to lock (stays on until tapped again).
// ---------------------------------------------------------------------------

type ModifierState = "off" | "armed" | "locked";

function cycleModifier(state: ModifierState): ModifierState {
  switch (state) {
    case "off": return "armed";
    case "armed": return "locked";
    case "locked": return "off";
  }
}

// ---------------------------------------------------------------------------
// (modifier, key) → byte-sequence table. Terminals do NOT simply apply a
// modifier bit to the byte: shift-tab is CSI Z (0x1b 0x5b 0x5a), not
// 0x09 with a flag. Ctrl-letter is byte minus 0x40 by convention. Arrows
// with modifiers use CSI 1;N notation.
// ---------------------------------------------------------------------------

interface AccessoryKey {
  label: string;
  /** What a bare press of this key sends. */
  bytes: number[];
  /** Shift variant. If absent, shift has no effect. */
  shiftBytes?: number[];
  /** Ctrl variant. If absent, ctrl has no effect. */
  ctrlBytes?: number[];
}

// Ctrl + letter sends byte = charCode - 0x40. Rather than hardcoding ^C,
// common control targets appear as keys whose ctrl variant is computed.
function ctrlLetterBytes(letter: string): number[] {
  return [letter.toUpperCase().charCodeAt(0) - 0x40];
}

const ACCESSORY_KEYS: AccessoryKey[] = [
  { label: "Esc", bytes: [0x1b] },
  {
    label: "Tab",
    bytes: [0x09],
    shiftBytes: [0x1b, 0x5b, 0x5a], // CSI Z = backtab
  },
  {
    label: "◀",
    bytes: [0x1b, 0x5b, 0x44],
    shiftBytes: [0x1b, 0x5b, 0x31, 0x3b, 0x32, 0x44],  // CSI 1;2D
    ctrlBytes:  [0x1b, 0x5b, 0x31, 0x3b, 0x35, 0x44],  // CSI 1;5D
  },
  {
    label: "▼",
    bytes: [0x1b, 0x5b, 0x42],
    shiftBytes: [0x1b, 0x5b, 0x31, 0x3b, 0x32, 0x42],  // CSI 1;2B
    ctrlBytes:  [0x1b, 0x5b, 0x31, 0x3b, 0x35, 0x42],  // CSI 1;5B
  },
  {
    label: "▲",
    bytes: [0x1b, 0x5b, 0x41],
    shiftBytes: [0x1b, 0x5b, 0x31, 0x3b, 0x32, 0x41],  // CSI 1;2A
    ctrlBytes:  [0x1b, 0x5b, 0x31, 0x3b, 0x35, 0x41],  // CSI 1;5A
  },
  {
    label: "▶",
    bytes: [0x1b, 0x5b, 0x43],
    shiftBytes: [0x1b, 0x5b, 0x31, 0x3b, 0x32, 0x43],  // CSI 1;2C
    ctrlBytes:  [0x1b, 0x5b, 0x31, 0x3b, 0x35, 0x43],  // CSI 1;5C
  },
  { label: "Enter", bytes: [0x0d] },
];

// Letters that appear in the accessory row when Ctrl is armed or locked.
// Ctrl + letter = byte value minus 0x40 (e.g. C → 0x03). This replaces
// the old hardcoded ^C key and extends to every common control character.
const CTRL_LETTER_KEYS = ["C", "D", "Z", "A", "E", "L", "R", "W", "U", "K"] as const;

/** Resolves the byte sequence to send for a key given the active modifiers. */
function resolveBytes(
  key: AccessoryKey,
  shift: ModifierState,
  ctrl: ModifierState,
): number[] {
  const shiftActive = shift !== "off";
  const ctrlActive = ctrl !== "off";

  // Ctrl takes precedence when both are active
  if (ctrlActive && key.ctrlBytes) return key.ctrlBytes;
  if (shiftActive && key.shiftBytes) return key.shiftBytes;

  return key.bytes;
}

function formatMetrics(msg: {
  renderer: string;
  outputBytes: number;
  lastEchoMs: number | null;
  inputBytesSent?: number;
  inputBytesDropped?: number;
  bufferType?: "normal" | "alternate";
  mouseTracking?: string;
  termCols?: number;
  termRows?: number;
  lastSentCols?: number | null;
  lastSentRows?: number | null;
  ptyCols?: number | null;
  ptyRows?: number | null;
  lastReplaySnapshot?: boolean;
}): string {
  // Width diagnostic: the three numbers that name the culprit.
  // term=xterm grid, sent=last sendResize, pty=daemon echo.
  // If all three agree, the PTY and xterm are in sync.
  let line = "";
  if (msg.termCols !== undefined) {
    const tc = msg.termCols;
    const sc = msg.lastSentCols ?? "–";
    const pc = msg.ptyCols ?? "–";
    const match = msg.termCols === msg.ptyCols ? "OK" : "MISMATCH";
    line += `cols: term=${tc} sent=${sc} pty=${pc} [${match}]`;
    if (msg.lastReplaySnapshot) line += " snap";
    line += "\n";
  }
  line += `${msg.renderer} · out ${(msg.outputBytes / 1024).toFixed(1)} KiB`;
  if (msg.inputBytesSent === undefined) {
    line += " · in n/a (old bundle)";
  } else {
    line += ` · in ${(msg.inputBytesSent / 1024).toFixed(1)} KiB`;
    if (msg.inputBytesDropped) line += ` (dropped ${msg.inputBytesDropped})`;
  }
  if (msg.lastEchoMs !== null) line += ` · echo ${msg.lastEchoMs} ms`;
  if (msg.bufferType) {
    line += ` · ${msg.bufferType === "alternate" ? "alt" : "main"} screen`;
    if (msg.mouseTracking && msg.mouseTracking !== "none") line += `, mouse ${msg.mouseTracking}`;
  }
  return line;
}

export function TerminalScreen({
  config,
  target,
  client,
  auth,
  onBack,
  onSwitchTerminal,
  onOpenPlan,
}: {
  config: ControllerConfig;
  target: TerminalTarget;
  client: PmClient;
  auth: DeviceAuthSession;
  onBack: () => void;
  onSwitchTerminal: (target: TerminalTarget) => void;
  onOpenPlan?: (target: PlanTarget) => void;
}) {
  const webviewRef = useRef<WebView>(null);
  const swiftTermRef = useRef<SwiftTermSurfaceHandle>(null);
  const [status, setStatusRaw] = useState("loading");
  const graceRef = useRef(createStatusGrace(setStatusRaw));
  const setStatus = graceRef.current.setStatus;
  const [veiled, setVeiled] = useState(true);
  const veilRef = useRef(createReplayVeil(setVeiled, () => {
    void swiftTermRef.current?.revealAtBottom().then(() => {
      veilRef.current.event("timeout");
    });
  }));
  const [detail, setDetail] = useState<string | null>(null);
  const [mintFailed, setMintFailed] = useState(false);
  const [sizeMismatch, setSizeMismatch] = useState(false);
  const [metrics, setMetrics] = useState<string | null>(null);
  const [diagVisible, setDiagVisible] = useState(false);
  const [diagData, setDiagData] = useState<{
    // Shell-side measurements
    windowHeight: number;
    contentBottomY: number;
    kbScreenY: number;
    computedOverlap: number;
    insetApplied: number;
    webviewFrameHeight: number;
    // Page-side measurements
    layoutHeightPx: number;
    visualHeightPx: number;
    containerHeightPx: number;
    cols: number;
    rows: number;
    terminalPixelHeight: number;
    // Phase
    phase: "show" | "hide" | "idle";
  } | null>(null);
  const diagRef = useRef(diagData);
  diagRef.current = diagData;
  const accessoryRow = useRef(new AccessoryRowVisibility()).current;
  const [accessoryVisible, setAccessoryVisible] = useState(accessoryRow.visible());
  const keyboardVisibleRef = useRef(false);

  const [menuOpen, setMenuOpen] = useState(false);
  const [followingBottom, setFollowingBottom] = useState(true);
  const [sheetTab, setSheetTab] = useState<"glance" | "info" | "timeline" | "urls" | "terminals" | null>(null);
  const [hasSelection, setHasSelection] = useState(false);
  const hasSelectionRef = useRef(false);
  const selectionModeRef = useRef(false);
  const selectionTouchRef = useRef(false);
  const bracketedPasteRef = useRef(false);

  const appState = useSyncExternalStore(client.subscribe, client.getState);

  // All terminals for this session, from the snapshot.
  const sessionTerminals = useMemo(() => {
    const result: Terminal[] = [];
    for (const terminal of appState.terminals.values()) {
      if (terminal.sessionId === target.sessionId) result.push(terminal);
    }
    return result;
  }, [appState.terminals, target.sessionId]);

  const sessionPlans = useMemo(() => {
    const result: Plan[] = [];
    for (const plan of appState.plans.values()) {
      if (plan.owningSessionId === target.sessionId) result.push(plan);
    }
    return result;
  }, [appState.plans, target.sessionId]);

  const activeDecisionPlan = useMemo(
    () => sessionPlans.find((p) => p.state === PlanState.ACTIVE && p.activeDecisionId !== undefined),
    [sessionPlans],
  );

  const handleCreateShell = useCallback(async () => {
    try {
      const outcome = await client.createShell(target.sessionId);
      if (outcome.createdId) {
        const created = appState.terminals.get(outcome.createdId.toString());
        onSwitchTerminal({
          sessionId: target.sessionId,
          terminalId: outcome.createdId,
          generation: created?.generation ?? 1n,
          title: target.title,
        });
      }
      setSheetTab(null);
    } catch (err) {
      Alert.alert("Create shell failed", err instanceof Error ? err.message : String(err));
    }
  }, [client, target, appState.terminals, onSwitchTerminal]);

  const handleCloseTerminal = useCallback(async (terminalId: bigint) => {
    Alert.alert(
      "Close shell?",
      "This ends the shell process. Scrollback remains available.",
      [
        { text: "Cancel", style: "cancel" },
        {
          text: "Close",
          style: "destructive",
          onPress: async () => {
            try {
              await client.closeTerminal(terminalId);
              // If we closed the terminal we are viewing, switch to agent
              if (terminalId === target.terminalId) {
                const agentTerminal = sessionTerminals.find(
                  (t) => t.kind === TerminalKind.AGENT,
                );
                if (agentTerminal) {
                  onSwitchTerminal({
                    sessionId: target.sessionId,
                    terminalId: agentTerminal.id,
                    generation: agentTerminal.generation,
                    title: target.title,
                  });
                }
              }
              setSheetTab(null);
            } catch (err) {
              Alert.alert("Close failed", err instanceof Error ? err.message : String(err));
            }
          },
        },
      ],
    );
  }, [client, target, sessionTerminals, onSwitchTerminal]);

  const forwardGroups = useMemo(
    () => forwardsForSession(
      { sessions: appState.sessions, forwards: appState.forwards },
      target.sessionId.toString(),
    ),
    [appState.sessions, appState.forwards, target.sessionId],
  );

  const openForward = useCallback(async (forward: SessionForward) => {
    if (!forward.url) {
      Alert.alert(
        "Forward unavailable",
        "This forward is stopped or the controller cannot name a reachable URL. Resume its session and check the controller's public URL.",
      );
      return;
    }
    try {
      const url = await auth.withAccessToken(config.baseUrl, (accessToken) =>
        openableForwardUrl(bearerJsonFetch(config.baseUrl, accessToken, fetch), forward.id.toString(), forward.url),
      );
      await Linking.openURL(url);
    } catch (err) {
      Alert.alert("Forward", `Could not open forward: ${err instanceof Error ? err.message : String(err)}`);
    }
  }, [auth, config.baseUrl]);

  const handleInterrupt = useCallback(async () => {
    try {
      await client.interruptSession(target.sessionId);
    } catch (err) {
      Alert.alert("Interrupt failed", err instanceof Error ? err.message : String(err));
    }
  }, [client, target.sessionId]);

  const handleKill = useCallback(async () => {
    try {
      await client.killSession(target.sessionId);
    } catch (err) {
      Alert.alert("Kill failed", err instanceof Error ? err.message : String(err));
    }
  }, [client, target.sessionId]);

  const confirmKill = useCallback(() => {
    Alert.alert(
      `Kill "${target.title}"?`,
      "Stops the agent process and ends the session. This cannot be undone.",
      [
        { text: "Cancel", style: "cancel" },
        { text: "Kill", style: "destructive", onPress: handleKill },
      ],
    );
  }, [target.title, handleKill]);

  const handleOpenPlan = useCallback((plan: Plan) => {
    onOpenPlan?.({
      planId: plan.id.toString(),
      sessionId: target.sessionId,
      planName: plan.name,
    });
  }, [onOpenPlan, target.sessionId]);

  const terminalMenuItems: OverflowMenuItem[] = useMemo(() => {
    const items: OverflowMenuItem[] = [];

    if (activeDecisionPlan) {
      items.push({
        key: `plan-active-${activeDecisionPlan.id}`,
        label: `${activeDecisionPlan.name} (needs input)`,
        icon: "dot",
        color: colors.amber,
        onPress: () => handleOpenPlan(activeDecisionPlan),
      });
    }
    for (const plan of sessionPlans) {
      if (plan === activeDecisionPlan) continue;
      items.push({
        key: `plan-${plan.id}`,
        label: plan.name,
        icon: "dot",
        onPress: () => handleOpenPlan(plan),
      });
    }

    items.push(
      { key: "terminals", label: "Terminals", icon: "list", onPress: () => setSheetTab("terminals") },
      { key: "info", label: "Session info", icon: "info", onPress: () => setSheetTab("info") },
      { key: "glance", label: "Glance & context", icon: "list", onPress: () => setSheetTab("glance") },
      { key: "timeline", label: "Timeline", icon: "clock", onPress: () => setSheetTab("timeline") },
      { key: "urls", label: "Shared URLs", icon: "link", onPress: () => setSheetTab("urls") },
      { key: "copyId", label: "Copy session id", icon: "copy", onPress: () => {
        Clipboard.setString(target.sessionId.toString());
      } },
      { key: "interrupt", label: "Interrupt", icon: "warning", color: colors.amber, onPress: handleInterrupt },
      { key: "kill", label: "Kill session", icon: "x", color: colors.red, onPress: confirmKill },
    );
    return items;
  }, [target.sessionId, handleInterrupt, confirmKill, sessionPlans, activeDecisionPlan, handleOpenPlan]);

  // The accessory row follows the software keyboard, which appears for
  // WebView focus without resizing the RN layout on iOS. Android resizes
  // the window itself. The header toggle summons the row without the
  // keyboard and dismisses it while typing.
  useEffect(() => {
    const shown = () => {
      keyboardVisibleRef.current = true;
      accessoryRow.keyboardShown();
      setAccessoryVisible(accessoryRow.visible());
    };
    const hidden = () => {
      keyboardVisibleRef.current = false;
      accessoryRow.keyboardHidden();
      setAccessoryVisible(accessoryRow.visible());
    };
    const subscriptions =
      Platform.OS === "ios"
        ? [Keyboard.addListener("keyboardWillShow", shown), Keyboard.addListener("keyboardWillHide", hidden)]
        : [Keyboard.addListener("keyboardDidShow", shown), Keyboard.addListener("keyboardDidHide", hidden)];
    return () => subscriptions.forEach((subscription) => subscription.remove());
  }, [accessoryRow]);

  const toggleAccessory = () => {
    if (keyboardVisibleRef.current) {
      Keyboard.dismiss();
      return;
    }
    if (Platform.OS === "ios") {
      swiftTermRef.current?.focus();
      return;
    }
    setAccessoryVisible(accessoryRow.toggle());
  };

  // Live generation from the control socket state; a restarted terminal
  // invalidates outstanding tickets, so the attach controller re-mints.
  const generation = useSyncExternalStore(client.subscribe, () => {
    const terminal = client.getState().terminals.get(target.terminalId.toString());
    return (terminal?.generation ?? target.generation).toString();
  });

  const send = useCallback((msg: HostMessage) => {
    if (Platform.OS === "ios") {
      swiftTermRef.current?.send(msg);
    } else {
      webviewRef.current?.postMessage(encodeHostMessage(msg));
    }
  }, []);

  const handleCopy = useCallback(() => {
    send({ type: "getSelection" });
  }, [send]);

  const handlePaste = useCallback(async () => {
    const text = await Clipboard.getString();
    if (!text) return;
    const encoder = new TextEncoder();
    const useBracket = bracketedPasteRef.current;
    const payload = useBracket ? BRACKETED_PASTE_START + text + BRACKETED_PASTE_END : text;
    send({ type: "write", dataBase64: bytesToBase64(encoder.encode(payload)) });
  }, [send]);

  // The content sits inside SafeAreaView, so its bottom edge is above the
  // home indicator. Keyboard overlap must be measured against this edge —
  // not the full window height — to avoid over-compensating by the bottom
  // safe-area inset.
  const rootRef = useRef<View>(null);
  const contentBottomY = useRef(Dimensions.get("window").height);
  const measureContentBottom = useCallback(() => {
    rootRef.current?.measureInWindow((_x, y, _w, h) => {
      if (h > 0) contentBottomY.current = y + h;
    });
  }, []);

  // Touch coordinates cross the bridge in the WebView page's coordinate
  // space, so the frame's window origin is subtracted from pageX/pageY.
  const frameRef = useRef<View>(null);
  const frameOrigin = useRef({ x: 0, y: 0 });
  const measureFrameOrigin = useCallback(() => {
    frameRef.current?.measureInWindow((x, y) => {
      frameOrigin.current = { x, y };
    });
  }, []);

  const toPanSample = useCallback((evt: GestureResponderEvent): PanSample => {
    const { pageX, pageY, timestamp, touches } = evt.nativeEvent;
    return {
      x: pageX - frameOrigin.current.x,
      y: pageY - frameOrigin.current.y,
      timeMs: timestamp,
      touchCount: Math.max(touches.length, 1),
    };
  }, []);

  // Every raw touch is forwarded, claimed or not: the WebView's mouse
  // reporter needs the full stream to deliver taps and selection drags to
  // mouse-tracking applications, because WKWebView gives page JS no usable
  // mouse events for touches. Touch-move is coalesced per animation frame
  // to avoid flooding the bridge at 120 Hz on ProMotion displays.
  const pendingTouchMove = useRef<{ x: number; y: number; timeMs: number; touchCount: number } | null>(null);
  const touchMoveFrame = useRef(0);
  const forwardTouch = useCallback(
    (phase: PanPhase) => (evt: GestureResponderEvent) => {
      const sample = toPanSample(evt);
      if (phase === "start") {
        selectionTouchRef.current = selectionModeRef.current;
        if (selectionTouchRef.current) {
          // WebKit owns every touch while native selection is active. Do not
          // let the raw-touch reporter reinterpret dismissal or handle drags
          // as terminal taps that focus the keyboard.
          send({ type: "cancelTouch" });
          return;
        }
      } else if (selectionTouchRef.current) {
        if (phase === "end" || phase === "cancel") {
          selectionTouchRef.current = false;
          selectionModeRef.current = hasSelectionRef.current;
          requestAnimationFrame(() => send({ type: "finishSelectionTouch" }));
        }
        return;
      }
      if (phase === "move") {
        pendingTouchMove.current = { x: sample.x, y: sample.y, timeMs: sample.timeMs, touchCount: sample.touchCount };
        if (!touchMoveFrame.current) {
          touchMoveFrame.current = requestAnimationFrame(() => {
            touchMoveFrame.current = 0;
            const p = pendingTouchMove.current;
            if (p) {
              pendingTouchMove.current = null;
              send({ type: "touch", phase: "move", x: p.x, y: p.y, timeMs: p.timeMs, touchCount: p.touchCount });
            }
          });
        }
        return;
      }
      // Flush any buffered move before start/end/cancel.
      if (pendingTouchMove.current) {
        if (touchMoveFrame.current) {
          cancelAnimationFrame(touchMoveFrame.current);
          touchMoveFrame.current = 0;
        }
        const p = pendingTouchMove.current;
        pendingTouchMove.current = null;
        send({ type: "touch", phase: "move", x: p.x, y: p.y, timeMs: p.timeMs, touchCount: p.touchCount });
      }
      send({ type: "touch", phase, x: sample.x, y: sample.y, timeMs: sample.timeMs, touchCount: sample.touchCount });
    },
    [send, toPanSample],
  );
  const touchHandlers = useMemo(
    () => ({
      onTouchStart: forwardTouch("start"),
      onTouchMove: forwardTouch("move"),
      onTouchEnd: forwardTouch("end"),
      onTouchCancel: forwardTouch("cancel"),
    }),
    [forwardTouch],
  );

  // WKWebView's native recognizers consume drags before the page sees a
  // touchmove, so plain drags are claimed here and forwarded for the
  // WebView's gesture engine to apply. Taps, long-press selection, and
  // multi-touch gestures are never claimed. Claimed moves are coalesced
  // per animation frame to keep bridge traffic at display rate.
  // Pan moves are forwarded to the bridge without coalescing so the
  // gesture engine sees every sample for its velocity estimate. The
  // engine's own line-quantization already bounds the render rate —
  // sub-line moves produce no viewport update.
  const panResponder = useMemo(() => {
    const bridge = new TerminalPanBridge(send);

    return PanResponder.create({
      onStartShouldSetPanResponder: () => false,
      onStartShouldSetPanResponderCapture: (evt) => {
        bridge.touchStart(toPanSample(evt));
        return false;
      },
      onMoveShouldSetPanResponderCapture: (evt) => {
        // Once WebKit owns a native selection, its drag recognizer must keep
        // the gesture so the blue handles can be moved. Treating that drag as
        // terminal scroll makes the selection appear frozen.
        if (selectionModeRef.current) return false;
        const claimed = bridge.shouldClaim(toPanSample(evt));
        // RN can claim a drag before a WebView touchmove crosses the bridge.
        // Cancel the pending tap now so a late touchend cannot focus xterm
        // and summon the keyboard after scrolling.
        if (claimed) send({ type: "cancelTouch" });
        return claimed;
      },
      onPanResponderGrant: (evt) => bridge.grant(toPanSample(evt)),
      onPanResponderMove: (evt) => bridge.move(toPanSample(evt)),
      onPanResponderRelease: (evt) => bridge.release(toPanSample(evt)),
      onPanResponderTerminate: (evt) => bridge.terminate(toPanSample(evt)),
      onPanResponderTerminationRequest: () => false,
      onShouldBlockNativeResponder: () => true,
    });
  }, [send, toPanSample]);

  // The keyboard shift is a useNativeDriver transform that tracks the
  // keyboard animation — no page-side resize events per frame. The layout
  // inset (and the single WebView refit it causes) applies immediately at
  // keyboard-event time.
  const shiftY = useRef(new Animated.Value(0)).current;
  const [keyboardInsetPx, setKeyboardInsetPx] = useState(0);
  const planner = useRef(new KeyboardShiftPlanner()).current;

  const runShiftDirectives = useCallback(
    (directives: KeyboardShiftDirective[]) => {
      for (const directive of directives) {
        switch (directive.kind) {
          case "jump":
            shiftY.setValue(directive.toPx);
            break;
          case "inset":
            setKeyboardInsetPx(directive.px);
            break;
          case "animate":
            Animated.timing(shiftY, {
              toValue: directive.toPx,
              duration: directive.durationMs,
              easing: KEYBOARD_EASING,
              useNativeDriver: true,
            }).start(({ finished }) => {
              if (finished) runShiftDirectives(planner.animationEnded());
            });
            break;
        }
      }
    },
    [planner, send, shiftY],
  );

  const requestDiag = useCallback(() => {
    send({ type: "requestDiag" });
  }, [send]);

  useEffect(() => {
    if (Platform.OS !== "ios") return;
    const shown = Keyboard.addListener("keyboardWillShow", (event: KeyboardEvent) => {
      // Use the measured content bottom (inside SafeAreaView) instead of the
      // full window height, so the overlap excludes the bottom safe-area
      // inset that SafeAreaView already accounts for.
      const overlap = contentBottomY.current - event.endCoordinates.screenY;
      if (__DEV__) {
        const windowH = Dimensions.get("window").height;
        setDiagData({
          windowHeight: windowH,
          contentBottomY: contentBottomY.current,
          kbScreenY: event.endCoordinates.screenY,
          computedOverlap: overlap,
          insetApplied: Math.max(0, Math.round(overlap)),
          webviewFrameHeight: frameHeight.current,
          layoutHeightPx: 0,
          visualHeightPx: 0,
          containerHeightPx: 0,
          cols: 0,
          rows: 0,
          terminalPixelHeight: 0,
          phase: "show",
        });
        // Request the page side after a short delay to let the refit settle
        setTimeout(requestDiag, 400);
      }
      veilRef.current.event("kbShow");
      runShiftDirectives(planner.willShow(overlap, event.duration));
    });
    const hidden = Keyboard.addListener("keyboardWillHide", (event: KeyboardEvent) => {
      if (__DEV__) {
        setDiagData((prev) =>
          prev
            ? { ...prev, phase: "hide", insetApplied: 0, computedOverlap: 0, kbScreenY: 0 }
            : null,
        );
        setTimeout(requestDiag, 400);
      }
      runShiftDirectives(planner.willHide(event.duration));
    });
    return () => {
      shown.remove();
      hidden.remove();
    };
  }, [planner, runShiftDirectives, requestDiag]);

  // iOS: bearer-token path — the access token authenticates every connection
  // including reconnects, so no ticket is needed.
  // Android: ticket path — TerminalAttachController mints one-use tickets.
  const useBearer = Platform.OS === "ios";

  const sendBearerInit = useCallback(
    async (gen: string) => {
      veilRef.current.event("init");
      setMintFailed(false);
      setStatus("authorizing");
      setDetail(null);
      try {
        // Await the token and the first native surface size in parallel so
        // the network wait overlaps the layout, and the single init message
        // carries both the bearer flag and the phone's dimensions.
        const [token, size] = await Promise.all([
          auth.freshAccessToken(config.baseUrl),
          swiftTermRef.current?.firstSize() ?? Promise.resolve(null),
        ]);
        send({
          type: "init",
          socketBaseUrl: websocketBaseUrl(config.baseUrl),
          terminalId: target.terminalId.toString(),
          generation: gen,
          accessToken: token,
          initialCols: size?.cols,
          initialRows: size?.rows,
          replayCapBytes: TERMINAL_REPLAY_CAP_BYTES,
          fontSize: TERMINAL_FONT_SIZE,
        });
      } catch (err) {
        if (veilRef.current.veiled()) {
          void swiftTermRef.current?.revealAtBottom().then(() => {
            veilRef.current.event("error");
          });
        } else {
          veilRef.current.event("error");
        }
        setMintFailed(true);
        setStatus("rejected");
        setDetail(`could not authorize terminal: ${err instanceof Error ? err.message : String(err)}`);
      }
    },
    [auth, config.baseUrl, send, target.terminalId],
  );

  const controller = useMemo(
    () =>
      useBearer
        ? null
        : new TerminalAttachController(
            {
              mintTicket: (gen) =>
                auth
                  .withAccessToken(config.baseUrl, (token) =>
                    mintTerminalAttachTicket(
                      config.baseUrl,
                      token,
                      target.terminalId.toString(),
                      gen,
                      fetch,
                      TERMINAL_REPLAY_CAP_BYTES,
                    ),
                  )
                  .then((ticket) => ticket.ticket),
              sendInit: (ticket, gen) => {
                // On Android the WebView reports its own size through the
                // existing refit path, so initialCols/Rows are not needed.
                veilRef.current.event("init");
                send({
                  type: "init",
                  socketBaseUrl: websocketBaseUrl(config.baseUrl),
                  terminalId: target.terminalId.toString(),
                  generation: gen,
                  ticket,
                  replayCapBytes: TERMINAL_REPLAY_CAP_BYTES,
                  fontSize: TERMINAL_FONT_SIZE,
                });
              },
              onPhase: (phase) => {
                switch (phase.kind) {
                  case "minting":
                    setMintFailed(false);
                    setStatus("authorizing");
                    setDetail(null);
                    break;
                  case "error":
                    if (veilRef.current.veiled()) {
                      void swiftTermRef.current?.revealAtBottom().then(() => {
                        veilRef.current.event("error");
                      });
                    } else {
                      veilRef.current.event("error");
                    }
                    setMintFailed(true);
                    setStatus("rejected");
                    setDetail(`could not authorize terminal: ${phase.reason}`);
                    break;
                  case "initSent":
                  case "idle":
                    break;
                }
              },
            },
            target.generation.toString(),
          ),
    [useBearer, auth, config.baseUrl, send, target.terminalId, target.generation],
  );

  // Bearer path: track the latest generation so ready/retry can use it.
  const bearerGenRef = useRef(target.generation.toString());
  useEffect(() => {
    bearerGenRef.current = generation;
    if (useBearer) void sendBearerInit(generation);
  }, [useBearer, generation, sendBearerInit]);

  useEffect(() => {
    controller?.setGeneration(generation);
  }, [controller, generation]);

  useEffect(() => {
    return () => {
      controller?.dispose();
      send({ type: "shutdown" });
      graceRef.current.dispose();
      veilRef.current.dispose();
    };
  }, [controller, send]);

  // Install the bearer header provider so every WebSocket open reads the
  // current access token, not the one captured at init time.
  useEffect(() => {
    if (!useBearer) return;
    swiftTermRef.current?.setHeaderProvider((): Record<string, string> => {
      const token = auth.accessToken();
      if (token) return { Authorization: `Bearer ${token}` };
      return {};
    });
    return () => swiftTermRef.current?.setHeaderProvider(null);
  }, [useBearer, auth]);

  useEffect(() => bindTerminalVisibility(AppState, send), [send]);

  const handleViewMessage = (msg: ViewMessage) => {
    switch (msg.type) {
      case "sizeMismatch":
        setSizeMismatch(msg.show);
        break;
      case "ready":
        // Bearer path: init was already sent on mount (the useEffect on
        // generation fires first), so ready is a no-op for connection.
        // Ticket path: the WebView loaded; start the ticket flow.
        if (!useBearer) controller!.viewReady();
        break;
      case "replayPainted":
        void swiftTermRef.current?.revealAtBottom().then(() => {
          veilRef.current.event("painted");
        });
        break;
      case "nativeResized":
        veilRef.current.event("kbSettle");
        break;
      case "status":
        setStatus(msg.phase);
        setDetail(msg.detail ?? null);
        if (msg.phase === "online") {
          if (veilRef.current.veiled()) {
            void swiftTermRef.current?.revealAtBottom().then(() => {
              veilRef.current.event("online");
            });
          } else {
            veilRef.current.event("online");
          }
        }
        if (useBearer) {
          // The header provider already presents the current token on every
          // retry, so a re-init is only needed when freshAccessToken actually
          // rotated the token. A gratuitous re-init would tear down the
          // wrapper's socket and reset its exponential backoff.
          if (msg.phase === "reconnecting") {
            void shouldBearerReInit(
              () => auth.accessToken(),
              () => auth.freshAccessToken(config.baseUrl),
            ).then((needed) => {
              if (needed) void sendBearerInit(bearerGenRef.current);
            });
          }
        } else {
          controller!.viewStatus(msg.phase);
        }
        break;
      case "replay":
        veilRef.current.event("replay");
        setMetrics(
          `replay ${(msg.bytes / 1024).toFixed(1)} KiB in ${msg.durationMs} ms${msg.capped ? " (capped)" : ""}`,
        );
        break;
      case "metrics":
        setMetrics(formatMetrics(msg));
        break;
      case "error":
        if (veilRef.current.veiled()) {
          void swiftTermRef.current?.revealAtBottom().then(() => {
            veilRef.current.event("error");
          });
        } else {
          veilRef.current.event("error");
        }
        setStatus("rejected");
        setDetail(msg.message);
        if (useBearer) {
          void sendBearerInit(bearerGenRef.current);
        } else {
          controller!.authRejected();
        }
        break;
      case "keyboardDiag":
        setDiagData((prev) =>
          prev
            ? {
                ...prev,
                layoutHeightPx: msg.layoutHeightPx,
                visualHeightPx: msg.visualHeightPx,
                containerHeightPx: msg.containerHeightPx,
                cols: msg.cols,
                rows: msg.rows,
                terminalPixelHeight: msg.terminalPixelHeight,
              }
            : null,
        );
        break;
      case "following":
        setFollowingBottom(msg.following);
        break;
      case "selection":
        hasSelectionRef.current = msg.active;
        if (msg.active) selectionModeRef.current = true;
        setHasSelection(msg.active);
        if (msg.bracketedPasteMode !== undefined) {
          bracketedPasteRef.current = msg.bracketedPasteMode;
        }
        break;
      case "selectionText":
        if (msg.text) Clipboard.setString(msg.text);
        break;
      case "title":
      case "link":
        break;
    }
  };

  const onWebViewMessage = (event: WebViewMessageEvent) => {
    const msg = parseViewMessage(event.nativeEvent.data);
    if (msg) handleViewMessage(msg);
  };

  // Triple-tap on the status label toggles the diagnostic overlay in dev builds.
  const diagTapCount = useRef(0);
  const diagTapTimer = useRef<ReturnType<typeof setTimeout> | null>(null);
  const toggleDiag = useCallback(() => {
    if (!__DEV__) return;
    diagTapCount.current += 1;
    if (diagTapTimer.current) clearTimeout(diagTapTimer.current);
    if (diagTapCount.current >= 3) {
      diagTapCount.current = 0;
      setDiagVisible((v) => !v);
      if (!diagRef.current) {
        // Seed with shell-side data
        setDiagData({
          windowHeight: Dimensions.get("window").height,
          contentBottomY: contentBottomY.current,
          kbScreenY: 0,
          computedOverlap: 0,
          insetApplied: keyboardInsetPx,
          webviewFrameHeight: frameHeight.current,
          layoutHeightPx: 0,
          visualHeightPx: 0,
          containerHeightPx: 0,
          cols: 0,
          rows: 0,
          terminalPixelHeight: 0,
          phase: "idle",
        });
        requestDiag();
      }
    } else {
      diagTapTimer.current = setTimeout(() => {
        diagTapCount.current = 0;
      }, 500);
    }
  }, [keyboardInsetPx, requestDiag]);

  const [shiftState, setShiftState] = useState<ModifierState>("off");
  const [ctrlState, setCtrlState] = useState<ModifierState>("off");

  const pressModifier = (
    state: ModifierState,
    setState: (s: ModifierState) => void,
  ) => {
    setState(cycleModifier(state));
  };

  const pressKey = (key: AccessoryKey) => {
    const bytes = resolveBytes(key, shiftState, ctrlState);
    send({ type: "write", dataBase64: bytesToBase64(new Uint8Array(bytes)) });
    // Disarm one-shot modifiers after a key press
    if (shiftState === "armed") setShiftState("off");
    if (ctrlState === "armed") setCtrlState("off");
  };

  const pressCtrlLetter = (letter: string) => {
    send({ type: "write", dataBase64: bytesToBase64(new Uint8Array(ctrlLetterBytes(letter))) });
    if (ctrlState === "armed") setCtrlState("off");
  };

  // WKWebView does not reliably relayout its page when the hosting view
  // changes size, so every frame size change is also reported explicitly
  // for the page to refit and resize the PTY.
  const frameHeight = useRef(0);
  const onFrameLayout = (event: LayoutChangeEvent) => {
    measureFrameOrigin();
    const height = Math.round(event.nativeEvent.layout.height);
    if (height === frameHeight.current) return;
    frameHeight.current = height;
    send({ type: "refit" });
  };

  return (
    <View ref={rootRef} style={styles.root} onLayout={measureContentBottom}>
      <View style={styles.header}>
        <Pressable onPress={onBack} style={styles.backTouch} accessibilityLabel="Back to sessions">
          <View style={styles.backChevron} />
        </Pressable>
        <Text style={styles.title} numberOfLines={1}>
          {target.title}
        </Text>
        <Pressable onPress={toggleDiag} hitSlop={8}>
          <Text style={[styles.status, __DEV__ && diagVisible ? styles.diagActive : null]}>{status}</Text>
        </Pressable>
        <Pressable
          onPress={toggleAccessory}
          hitSlop={8}
          accessibilityRole="button"
          accessibilityLabel={accessoryVisible ? "Hide key row" : "Show key row"}
        >
          <Text style={[styles.keysToggle, accessoryVisible ? styles.keysToggleActive : null]}>⌨</Text>
        </Pressable>
        <OverflowButton onPress={() => setMenuOpen(true)} />
      </View>
      <OverflowMenu visible={menuOpen} items={terminalMenuItems} onClose={() => setMenuOpen(false)} />
      <SessionSheet
        visible={sheetTab !== null && sheetTab !== "terminals"}
        sessionId={target.sessionId}
        initialTab={sheetTab === "terminals" ? undefined : sheetTab ?? undefined}
        client={client}
        config={config}
        auth={auth}
        forwardGroups={forwardGroups}
        onOpenForward={openForward}
        onClose={() => setSheetTab(null)}
      />
      <TerminalsSheet
        visible={sheetTab === "terminals"}
        terminals={sessionTerminals}
        selectedTerminalId={target.terminalId}
        onSelect={(terminal) => {
          onSwitchTerminal({
            sessionId: target.sessionId,
            terminalId: terminal.id,
            generation: terminal.generation,
            title: target.title,
          });
          setSheetTab(null);
        }}
        onCreateShell={handleCreateShell}
        onClose={() => setSheetTab(null)}
        onCloseTerminal={handleCloseTerminal}
      />
      {detail ? <Text style={styles.detail}>{detail}</Text> : null}
      {mintFailed ? (
        <Pressable style={styles.retry} onPress={() => {
          if (useBearer) void sendBearerInit(bearerGenRef.current);
          else controller!.retry();
        }}>
          <Text style={styles.retryText}>Retry</Text>
        </Pressable>
      ) : null}
      <View style={[styles.shiftClip, { paddingBottom: keyboardInsetPx }]}>
        <Animated.View style={[styles.shifted, { transform: [{ translateY: shiftY }] }]}>
          <View style={styles.terminalArea}>
            <View
              ref={frameRef}
              style={styles.webviewFrame}
              onLayout={onFrameLayout}
              {...(Platform.OS === "ios" ? {} : touchHandlers)}
              {...(Platform.OS === "ios" ? {} : panResponder.panHandlers)}
            >
              {Platform.OS === "ios" ? (
                <SwiftTermSurface ref={swiftTermRef} onMessage={handleViewMessage} veiled={veiled} />
              ) : (
                <WebView
                  ref={webviewRef}
                  style={styles.webview}
                  originWhitelist={["*"]}
                  source={{ html: terminalHtml }}
                  onMessage={onWebViewMessage}
                  javaScriptEnabled
                  domStorageEnabled={false}
                  scrollEnabled={false}
                  showsVerticalScrollIndicator={false}
                  showsHorizontalScrollIndicator={false}
                  allowsBackForwardNavigationGestures={false}
                  setSupportMultipleWindows={false}
                  keyboardDisplayRequiresUserAction={false}
                  webviewDebuggingEnabled={__DEV__}
                />
              )}
            </View>
            {!followingBottom ? (
              <Pressable
                style={styles.scrollBottomButton}
                onPress={() => send({ type: "scrollToBottom" })}
                accessibilityRole="button"
                accessibilityLabel="Scroll to bottom"
              >
                <Text style={styles.scrollBottomArrow}>{">"}</Text>
              </Pressable>
            ) : null}
            {sizeMismatch ? (
              <View style={styles.sizePromptOverlay} pointerEvents="box-none">
              <View style={styles.sizePrompt} accessibilityLiveRegion="polite">
                <Text style={styles.sizePromptText}>This terminal is sized for another view. Resize it to fit this screen?</Text>
                <View style={styles.sizePromptActions}>
                  <Pressable accessibilityRole="button" accessibilityLabel="Update terminal size" onPress={() => send({ type: "updateSize" })}>
                    <Text style={styles.sizePromptAction}>Update</Text>
                  </Pressable>
                  <Pressable accessibilityRole="button" accessibilityLabel="Dismiss terminal size prompt" onPress={() => send({ type: "dismissSize" })}>
                    <Text style={styles.sizePromptAction}>Dismiss</Text>
                  </Pressable>
                </View>
              </View>
              </View>
            ) : null}
          </View>
          {accessoryVisible ? (
            <ScrollView
              horizontal
              keyboardShouldPersistTaps="always"
              style={styles.accessoryBar}
              contentContainerStyle={styles.accessoryContent}
            >
              <Pressable
                style={[
                  styles.key,
                  styles.modifierKey,
                  shiftState === "armed" && styles.modifierArmed,
                  shiftState === "locked" && styles.modifierLocked,
                ]}
                onPress={() => pressModifier(shiftState, setShiftState)}
              >
                <Text
                  style={[
                    styles.keyText,
                    shiftState !== "off" && styles.modifierKeyTextActive,
                  ]}
                >
                  {shiftState === "locked" ? "⇧⇧" : "⇧"}
                </Text>
              </Pressable>
              <Pressable
                style={[
                  styles.key,
                  styles.modifierKey,
                  ctrlState === "armed" && styles.modifierArmed,
                  ctrlState === "locked" && styles.modifierLocked,
                ]}
                onPress={() => pressModifier(ctrlState, setCtrlState)}
              >
                <Text
                  style={[
                    styles.keyText,
                    ctrlState !== "off" && styles.modifierKeyTextActive,
                  ]}
                >
                  {ctrlState === "locked" ? "Ctrl⁺" : "Ctrl"}
                </Text>
              </Pressable>
              {hasSelection ? (
                <Pressable style={styles.actionKey} onPress={handleCopy}>
                  <Text style={styles.actionKeyText}>Copy</Text>
                </Pressable>
              ) : null}
              <Pressable style={styles.actionKey} onPress={() => void handlePaste()}>
                <Text style={styles.actionKeyText}>Paste</Text>
              </Pressable>
              {ACCESSORY_KEYS.map((key) => (
                <Pressable key={key.label} style={styles.key} onPress={() => pressKey(key)}>
                  <Text style={styles.keyText}>{key.label}</Text>
                </Pressable>
              ))}
              {ctrlState !== "off" ? CTRL_LETTER_KEYS.map((letter) => (
                <Pressable key={`ctrl-${letter}`} style={styles.key} onPress={() => pressCtrlLetter(letter)}>
                  <Text style={styles.keyText}>^{letter}</Text>
                </Pressable>
              )) : null}
            </ScrollView>
          ) : null}
          {metrics ? <Text style={styles.metrics}>{metrics}</Text> : null}
        </Animated.View>
      </View>
      {__DEV__ && diagVisible && diagData ? (
        <View style={styles.diagOverlay}>
          <Text style={styles.diagTitle}>Keyboard Shift Diagnostics</Text>
          <Text style={styles.diagText}>Phase: {diagData.phase}</Text>
          <Text style={styles.diagSection}>--- Shell (RN) ---</Text>
          <Text style={styles.diagText}>windowHeight: {diagData.windowHeight.toFixed(1)}</Text>
          <Text style={styles.diagText}>contentBottomY: {diagData.contentBottomY.toFixed(1)}</Text>
          <Text style={styles.diagText}>kb endCoords.screenY: {diagData.kbScreenY.toFixed(1)}</Text>
          <Text style={styles.diagText}>
            computedOverlap: {diagData.computedOverlap.toFixed(1)} (clamped: {diagData.insetApplied})
          </Text>
          <Text style={styles.diagText}>paddingBottom (inset): {diagData.insetApplied}</Text>
          <Text style={styles.diagText}>webviewFrameHeight: {diagData.webviewFrameHeight}</Text>
          <Text style={styles.diagSection}>--- Page (WebView) ---</Text>
          <Text style={styles.diagText}>innerHeight: {diagData.layoutHeightPx}</Text>
          <Text style={styles.diagText}>visualViewport.height: {diagData.visualHeightPx}</Text>
          <Text style={styles.diagText}>container.clientHeight: {diagData.containerHeightPx}</Text>
          <Text style={styles.diagText}>
            terminal: {diagData.cols}x{diagData.rows} ({diagData.terminalPixelHeight}px)
          </Text>
          <Text style={styles.diagSection}>--- Gap Check ---</Text>
          <Text style={styles.diagText}>
            safeAreaGap: {(diagData.windowHeight - diagData.contentBottomY).toFixed(1)}
          </Text>
          <Text style={styles.diagText}>
            expectedFrameH: {(diagData.webviewFrameHeight - diagData.insetApplied).toFixed(1)} (after inset)
          </Text>
          <Pressable onPress={() => setDiagVisible(false)} style={styles.diagClose}>
            <Text style={styles.diagCloseText}>Close</Text>
          </Pressable>
        </View>
      ) : null}
    </View>
  );
}

const styles = StyleSheet.create({
  sizePromptOverlay: { ...StyleSheet.absoluteFillObject, justifyContent: "center", paddingHorizontal: 12 },
  sizePrompt: { padding: 12, borderRadius: 8, backgroundColor: "#202938", borderWidth: 1, borderColor: "#506078" },
  sizePromptText: { color: "#e8eaf0", fontSize: 13 },
  sizePromptActions: { flexDirection: "row", justifyContent: "flex-end", gap: 24, marginTop: 8 },
  sizePromptAction: { color: "#9dc7ff", fontSize: 14, paddingVertical: 6 },
  root: { flex: 1 },
  header: {
    flexDirection: "row",
    alignItems: "center",
    gap: 12,
    paddingHorizontal: 16,
    paddingVertical: 10,
  },
  backTouch: {
    width: 44,
    height: 44,
    alignItems: "center" as const,
    justifyContent: "center" as const,
    marginLeft: -12,
  },
  backChevron: {
    width: 10,
    height: 10,
    borderLeftWidth: 2.5,
    borderBottomWidth: 2.5,
    borderColor: colors.blue,
    transform: [{ rotate: "45deg" }],
  },
  title: { color: colors.textBright, fontSize: 15, fontWeight: "600", flex: 1 },
  status: { color: colors.textMuted, fontSize: 12 },
  keysToggle: { color: colors.textMuted, fontSize: 18 },
  keysToggleActive: { color: colors.blue },
  detail: { color: colors.amber, fontSize: 12, paddingHorizontal: 16, paddingBottom: 6 },
  retry: {
    alignSelf: "flex-start",
    marginHorizontal: 16,
    marginBottom: 6,
    backgroundColor: colors.panelAlt,
    borderRadius: 6,
    paddingHorizontal: 14,
    paddingVertical: 6,
  },
  retryText: { color: colors.blue, fontSize: 13 },
  shiftClip: { flex: 1, overflow: "hidden", backgroundColor: colors.bg },
  shifted: { flex: 1 },
  terminalArea: { flex: 1 },
  webviewFrame: { flex: 1 },
  webview: { flex: 1, backgroundColor: colors.bg },
  accessoryBar: { flexGrow: 0, backgroundColor: colors.panel },
  accessoryContent: { paddingHorizontal: 8, paddingVertical: 6, gap: 6 },
  actionKey: {
    backgroundColor: colors.blue,
    borderRadius: 6,
    paddingHorizontal: 14,
    paddingVertical: 8,
  },
  actionKeyText: { color: "#fff", fontSize: 14, fontWeight: "600" },
  key: {
    backgroundColor: colors.panelAlt,
    borderRadius: 6,
    paddingHorizontal: 14,
    paddingVertical: 8,
  },
  modifierKey: {
    borderWidth: 1,
    borderColor: "transparent",
  },
  modifierArmed: {
    backgroundColor: colors.surface,
    borderColor: colors.blue,
  },
  modifierLocked: {
    backgroundColor: colors.blue,
    borderColor: colors.blue,
  },
  keyText: { color: colors.text, fontSize: 14 },
  modifierKeyTextActive: { color: colors.textBright },
  metrics: { color: colors.textMuted, fontSize: 11, paddingHorizontal: 16, paddingVertical: 4 },
  scrollBottomButton: {
    position: "absolute",
    bottom: 16,
    right: 16,
    width: 44,
    height: 44,
    borderRadius: 22,
    backgroundColor: "rgba(61, 107, 214, 0.85)",
    alignItems: "center",
    justifyContent: "center",
    zIndex: 10,
  },
  scrollBottomArrow: {
    color: "#fff",
    fontSize: 20,
    lineHeight: 22,
    transform: [{ rotate: "90deg" }],
  },
  diagActive: { color: colors.amber },
  diagOverlay: {
    position: "absolute",
    bottom: 0,
    left: 0,
    right: 0,
    backgroundColor: "rgba(0, 0, 0, 0.92)",
    padding: 12,
    paddingBottom: 24,
    borderTopWidth: 1,
    borderTopColor: colors.amber,
  },
  diagTitle: { color: colors.amber, fontSize: 13, fontWeight: "700", marginBottom: 6 },
  diagSection: { color: colors.amber, fontSize: 11, marginTop: 6, marginBottom: 2 },
  diagText: { color: colors.text, fontSize: 11, fontFamily: "Menlo", lineHeight: 16 },
  diagClose: { marginTop: 8, alignSelf: "flex-end" },
  diagCloseText: { color: colors.blue, fontSize: 13 },
});
