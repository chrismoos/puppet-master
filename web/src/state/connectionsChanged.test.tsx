// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it } from "vitest";
import type { PmClient } from "@puppet-master/client-core/ws/client";
import { ClientContext, useConnectionsChanged } from "./hooks";

const listeners = new Set<() => void>();
const client = {
  onConnectionsChanged: (listener: () => void) => {
    listeners.add(listener);
    return () => listeners.delete(listener);
  },
} as unknown as PmClient;

let root: Root;
let host: HTMLDivElement;
let refetches = 0;

function Probe({ enabled }: { enabled: boolean }) {
  useConnectionsChanged(() => refetches++, enabled);
  return null;
}

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  refetches = 0;
  host = document.createElement("div");
  root = createRoot(host);
});
afterEach(() => {
  act(() => root.unmount());
  listeners.clear();
});

function render(enabled: boolean, withClient = true) {
  act(() =>
    root.render(
      withClient ? (
        <ClientContext.Provider value={client}>
          <Probe enabled={enabled} />
        </ClientContext.Provider>
      ) : (
        <Probe enabled={enabled} />
      ),
    ),
  );
}

it("refetches each time the client says connections changed", () => {
  render(true);
  for (const listener of listeners) listener();
  for (const listener of listeners) listener();
  expect(refetches).toBe(2);
});

it("unsubscribes when disabled or unmounted", () => {
  render(true);
  render(false);
  expect(listeners.size).toBe(0);
  render(true);
  act(() => root.unmount());
  root = createRoot(host);
  expect(listeners.size).toBe(0);
});

it("does nothing without a client, as in a static render", () => {
  render(true, false);
  expect(listeners.size).toBe(0);
});
