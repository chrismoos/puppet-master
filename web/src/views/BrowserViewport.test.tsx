// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { BrowserViewport } from "./BrowserViewport";

class FakeSocket {
  static sockets: FakeSocket[] = [];
  binaryType = "";
  onmessage?: (event: { data: unknown }) => void;
  onclose?: (event: { code: number }) => void;
  onerror?: () => void;
  close = vi.fn();
  constructor(readonly url: URL) { FakeSocket.sockets.push(this); }
}
let host: HTMLDivElement;
let root: Root;
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  FakeSocket.sockets = [];
  vi.stubGlobal("WebSocket", FakeSocket);
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(() => { act(() => root.unmount()); host.remove(); vi.restoreAllMocks(); vi.unstubAllGlobals(); });

it("shows browser status and disconnects when the pane is hidden", () => {
  const ready = vi.fn();
  act(() => root.render(<BrowserViewport path="pane-pages/token/stream" active onReady={ready} />));
  const socket = FakeSocket.sockets[0];
  expect(socket.url.pathname).toBe("/pane-pages/token/stream");
  act(() => socket.onmessage?.({ data: JSON.stringify({ state: "live", url: "about:blank", action: "click complete", width: 1280, height: 800 }) }));
  expect(host.textContent).toContain("Live · read-only");
  expect(host.textContent).toContain("click complete");
  expect(host.textContent).toContain("1280 × 800");
  act(() => root.render(<BrowserViewport path="pane-pages/token/stream" active={false} onReady={ready} />));
  expect(socket.close).toHaveBeenCalledOnce();
  expect(host.textContent).toContain("Paused");
  expect(host.querySelector("canvas")?.getAttribute("aria-label")).toBe("Live agent browser");
});

it("offers reconnection when the browser context closes", () => {
  act(() => root.render(<BrowserViewport path="pane-pages/token/stream" active onReady={() => {}} />));
  act(() => FakeSocket.sockets[0].onclose?.({ code: 1000 }));
  expect(host.textContent).toContain("Browser stream disconnected");
  act(() => host.querySelector<HTMLButtonElement>("button")!.click());
  expect(FakeSocket.sockets).toHaveLength(2);
});

it("pauses delivery while the dashboard tab is hidden and resumes when visible", () => {
  const visibility = vi.spyOn(document, "visibilityState", "get").mockReturnValue("visible");
  act(() => root.render(<BrowserViewport path="pane-pages/token/stream" active onReady={() => {}} />));
  const socket = FakeSocket.sockets[0];
  visibility.mockReturnValue("hidden");
  act(() => document.dispatchEvent(new Event("visibilitychange")));
  expect(socket.close).toHaveBeenCalledOnce();
  expect(host.textContent).toContain("Paused");
  visibility.mockReturnValue("visible");
  act(() => document.dispatchEvent(new Event("visibilitychange")));
  expect(FakeSocket.sockets).toHaveLength(2);
});

it("keeps the last image during retries and bounds reconnects even when each socket sends status", () => {
  vi.useFakeTimers();
  try {
    act(() => root.render(<BrowserViewport path="pane-pages/token/stream" active onReady={() => {}} />));
    const retryDelay = 1500;
    const maximumConnections = 4;
    for (let attempt = 0; attempt < maximumConnections; attempt++) {
      const socket = FakeSocket.sockets.at(-1)!;
      act(() => socket.onmessage?.({ data: JSON.stringify({ state: "live" }) }));
      act(() => socket.onclose?.({ code: 1006 }));
      if (attempt < maximumConnections - 1) {
        expect(host.querySelector(".browser-disconnected")).toBeNull();
        expect(host.textContent).toContain("Reconnecting");
        act(() => vi.advanceTimersByTime(retryDelay));
      }
    }
    expect(FakeSocket.sockets).toHaveLength(maximumConnections);
    expect(host.textContent).toContain("Browser stream disconnected");
    act(() => vi.advanceTimersByTime(retryDelay * maximumConnections));
    expect(FakeSocket.sockets).toHaveLength(maximumConnections);
  } finally { vi.useRealTimers(); }
});
