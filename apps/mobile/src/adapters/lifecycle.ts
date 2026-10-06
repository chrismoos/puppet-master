export type AppStateStatus = "active" | "background" | "inactive" | "unknown" | "extension";

/** The subset of React Native's AppState the lifecycle adapter drives. */
export interface AppStateSource {
  currentState: AppStateStatus;
  addEventListener(type: "change", listener: (state: AppStateStatus) => void): { remove(): void };
}

/**
 * iOS reports "inactive" during transitions, and both it and "background"
 * count as backgrounded, matching the close-sockets-on-background design.
 */
export function isForeground(state: AppStateStatus): boolean {
  return state === "active";
}

export interface ForegroundCallbacks {
  onForeground(): void;
  onBackground(): void;
}

/** Collapses app-state churn into single foreground/background edges. */
export function watchForeground(source: AppStateSource, callbacks: ForegroundCallbacks): () => void {
  let foreground = isForeground(source.currentState);
  const subscription = source.addEventListener("change", (state) => {
    const nowForeground = isForeground(state);
    if (nowForeground === foreground) return;
    foreground = nowForeground;
    if (nowForeground) callbacks.onForeground();
    else callbacks.onBackground();
  });
  return () => subscription.remove();
}

export interface StartStopClient {
  start(): void;
  stop(): void;
}

/**
 * Runs the control client only while the app is foregrounded. Backgrounding
 * stops the client (closing control and terminal sockets); resume starts it
 * again, which reconnects and takes a fresh snapshot.
 */
export function bindClientToForeground(client: StartStopClient, source: AppStateSource): () => void {
  if (isForeground(source.currentState)) client.start();
  const unsubscribe = watchForeground(source, {
    onForeground: () => client.start(),
    onBackground: () => client.stop(),
  });
  return () => {
    unsubscribe();
    client.stop();
  };
}
