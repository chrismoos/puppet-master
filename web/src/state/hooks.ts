import { createContext, useContext, useEffect, useRef, useState, useSyncExternalStore } from "react";
import type { PmClient } from "@puppet-master/client-core/ws/client";
import type { AppState } from "@puppet-master/client-core/state/reducer";

export const ClientContext = createContext<PmClient | null>(null);

export function useClient(): PmClient {
  const client = useContext(ClientContext);
  if (!client) {
    throw new Error("ClientContext missing");
  }
  return client;
}

export function useAppState(): AppState {
  const client = useClient();
  // The server snapshot enables static renders, e.g. component tests.
  return useSyncExternalStore(client.subscribe, client.getState, client.getState);
}

/**
 * Calls `refetch` whenever connections, connection calls or approvals may
 * have changed, including after a reconnect. Without a client, as in a
 * static render, it never fires.
 */
export function useConnectionsChanged(refetch: () => void, enabled = true): void {
  const client = useContext(ClientContext);
  const latest = useRef(refetch);
  latest.current = refetch;
  useEffect(() => {
    if (!client || !enabled) return;
    return client.onConnectionsChanged(() => latest.current());
  }, [client, enabled]);
}

const TICK_INTERVAL_MS = 1_000;

/** Current time, refreshed once per second, for live elapsed displays. */
export function useNow(): number {
  const [now, setNow] = useState(() => Date.now());
  useEffect(() => {
    const timer = setInterval(() => setNow(Date.now()), TICK_INTERVAL_MS);
    return () => clearInterval(timer);
  }, []);
  return now;
}
