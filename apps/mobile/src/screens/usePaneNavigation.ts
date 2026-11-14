import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import { Alert, Animated, type GestureResponderEvent } from "react-native";

import type { Terminal } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { TerminalKind } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import type { AppState } from "@puppet-master/client-core/state/reducer";
import { insertPaneInTree } from "@puppet-master/client-core/state/paneMoves";
import {
  AGENT_ONLY_LAYOUT,
  AGENT_PANE,
  pagePanePath,
  parseSessionLayout,
  swipePanes,
  type SwipePane,
} from "@puppet-master/client-core/state/sessionLayout";

import { DoubleTap, PaneGestureRecognizer, paneAfter, type PaneGesture, type TouchPoint } from "../terminal/paneGesture";
import type { TerminalTarget } from "./SessionsScreen";

/** How long a pane's name shows after switching, before it fades. */
const FLASH_HOLD_MS = 900;
const FLASH_FADE_MS = 250;
/** How long a requested shell pane may take to appear before giving up on opening it. */
const PENDING_PANE_MS = 15_000;

export interface PaneNavigation {
  panes: SwipePane[];
  /** The pane on screen, as an index into `panes`, or -1 for a shell tab. */
  index: number;
  /** The page pane on screen, drawn over the terminal, when one is. */
  page: Extract<SwipePane, { kind: "page" }> | null;
  goTo: (index: number) => void;
  goToPage: (paneId: string) => void;
  flashText: string;
  flashOpacity: Animated.Value;
  touchHandlers: {
    onTouchStart: (event: GestureResponderEvent) => void;
    onTouchMove: (event: GestureResponderEvent) => void;
    onTouchEnd: (event: GestureResponderEvent) => void;
    onTouchCancel: () => void;
  };
  /** Counts taps on the header: a double tap goes back to the agent. */
  onHeaderPress: () => void;
  /** Opens a shell pane beside the agent, and switches to it once it starts. */
  newShellPane: () => void;
}

function points(event: GestureResponderEvent): TouchPoint[] {
  return event.nativeEvent.touches.map((touch) => ({
    id: Number(touch.identifier),
    x: touch.pageX,
    y: touch.pageY,
  }));
}

function paneLabel(pane: SwipePane): string {
  return pane.kind === "agent" ? "agent" : pane.title;
}

/** Swiping between a session's panes on the terminal screen: which panes
 * there are, which is on screen, and the gestures and flash that move
 * between them. */
export function usePaneNavigation({
  appState,
  target,
  sessionTerminals,
  onSwitchTerminal,
  createPane,
}: {
  appState: AppState;
  target: TerminalTarget;
  sessionTerminals: readonly Terminal[];
  onSwitchTerminal: (target: TerminalTarget) => void;
  /** Posts a new pane layout for the session. */
  createPane: (body: { id: string; base_rev: number; layout: unknown }) => Promise<void>;
}): PaneNavigation {
  const layout = useMemo(
    () => parseSessionLayout(appState.sessionLayouts.get(target.sessionId.toString())),
    [appState.sessionLayouts, target.sessionId],
  );
  const panes = useMemo(() => swipePanes(layout), [layout]);
  const agentTerminal = sessionTerminals.find((terminal) => terminal.kind === TerminalKind.AGENT);
  const [pageId, setPageId] = useState<string | null>(null);
  const page = pageId
    ? panes.find((pane): pane is Extract<SwipePane, { kind: "page" }> => pane.kind === "page" && pane.paneId === pageId) ?? null
    : null;
  const index = page
    ? panes.indexOf(page)
    : agentTerminal && target.terminalId === agentTerminal.id
      ? 0
      : panes.findIndex((pane) => pane.kind === "terminal" && pane.terminalId === target.terminalId.toString());

  // A page pane that closed while on screen hands the screen back to its terminal.
  useEffect(() => {
    if (pageId && !page) setPageId(null);
  }, [pageId, page]);

  const [flashText, setFlashText] = useState("");
  const flashOpacity = useRef(new Animated.Value(0)).current;
  const flash = useCallback((text: string) => {
    setFlashText(text);
    flashOpacity.stopAnimation();
    flashOpacity.setValue(1);
    Animated.timing(flashOpacity, {
      toValue: 0,
      delay: FLASH_HOLD_MS,
      duration: FLASH_FADE_MS,
      useNativeDriver: true,
    }).start();
  }, [flashOpacity]);

  const switchTo = useCallback((terminalId: string) => {
    const terminal = appState.terminals.get(terminalId);
    if (!terminal) return;
    onSwitchTerminal({
      sessionId: target.sessionId,
      terminalId: terminal.id,
      generation: terminal.generation,
      title: target.title,
    });
  }, [appState.terminals, onSwitchTerminal, target.sessionId, target.title]);

  const goTo = useCallback((next: number) => {
    const pane = panes[next];
    if (!pane) return;
    if (pane.kind === "page") {
      setPageId(pane.paneId);
    } else {
      setPageId(null);
      const terminalId = pane.kind === "agent" ? agentTerminal?.id.toString() : pane.terminalId;
      if (terminalId && terminalId !== target.terminalId.toString()) switchTo(terminalId);
    }
    flash(paneLabel(pane));
  }, [panes, agentTerminal, target.terminalId, switchTo, flash]);

  const goToPage = useCallback((paneId: string) => {
    const next = panes.findIndex((pane) => pane.kind === "page" && pane.paneId === paneId);
    if (next >= 0) goTo(next);
  }, [panes, goTo]);

  const onGesture = useCallback((gesture: PaneGesture) => {
    // A shell tab is not a pane, so swipes there lead nowhere.
    if (index < 0) return;
    const next = paneAfter(gesture, index, panes.length);
    if (next !== null) goTo(next);
  }, [index, panes.length, goTo]);

  const recognizer = useRef(new PaneGestureRecognizer()).current;
  const headerTaps = useRef(new DoubleTap()).current;
  const onHeaderPress = useCallback(() => {
    if (headerTaps.tap(Date.now()) && index !== 0) goTo(0);
  }, [headerTaps, index, goTo]);
  const touchHandlers = useMemo(() => ({
    onTouchStart: (event: GestureResponderEvent) => recognizer.start(points(event), event.nativeEvent.timestamp),
    onTouchMove: (event: GestureResponderEvent) => recognizer.move(points(event)),
    onTouchEnd: (event: GestureResponderEvent) => {
      const gesture = recognizer.end(points(event), event.nativeEvent.timestamp);
      if (gesture) onGesture(gesture);
    },
    onTouchCancel: () => recognizer.cancel(),
  }), [recognizer, onGesture]);

  const [pendingPane, setPendingPane] = useState<{ id: string; since: number } | null>(null);
  useEffect(() => {
    if (!pendingPane) return;
    const next = panes.findIndex((pane) => pane.kind === "terminal" && pane.paneId === pendingPane.id);
    if (next >= 0) {
      setPendingPane(null);
      goTo(next);
    } else if (Date.now() - pendingPane.since > PENDING_PANE_MS) {
      setPendingPane(null);
    }
  }, [panes, pendingPane, goTo]);

  const newShellPane = useCallback(() => {
    const current = layout ?? AGENT_ONLY_LAYOUT;
    let n = 1;
    while (current.panes[`shell-${n}`]) n += 1;
    const id = `shell-${n}`;
    const tree = insertPaneInTree(current.tree, id, AGENT_PANE, "right");
    setPendingPane({ id, since: Date.now() });
    createPane({ id, base_rev: current.rev, layout: tree }).catch((error: unknown) => {
      setPendingPane(null);
      Alert.alert("New shell pane failed", error instanceof Error ? error.message : String(error));
    });
  }, [layout, createPane]);

  return { panes, index, page, goTo, goToPage, flashText, flashOpacity, touchHandlers, onHeaderPress, newShellPane };
}

/** The address a page pane loads from, on the controller. */
export function pagePaneUrl(baseUrl: string, pane: Extract<SwipePane, { kind: "page" }>): string {
  const origin = /^[a-z][a-z0-9+.-]*:\/\/[^/]+/i.exec(baseUrl)?.[0] ?? baseUrl.replace(/\/+$/, "");
  return `${origin}/${pagePanePath(pane.pageToken, pane.version)}`;
}
