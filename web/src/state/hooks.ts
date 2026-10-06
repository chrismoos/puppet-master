import { createContext, useContext, useEffect, useState, useSyncExternalStore } from "react";
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
