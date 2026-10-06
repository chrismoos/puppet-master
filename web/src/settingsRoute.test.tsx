// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { useRoute } from "./router";

let root: Root;
let host: HTMLDivElement;
function Probe() {
  const route = useRoute();
  return <output data-route={JSON.stringify(route)} />;
}
function shown(): unknown {
  return JSON.parse(host.firstElementChild!.getAttribute("data-route")!);
}
function mount(path: string) {
  history.replaceState(null, "", `#${path}`);
  act(() => root.render(<Probe />));
}
function change(path: string) {
  const oldURL = location.href;
  history.pushState(null, "", `#${path}`);
  act(() => window.dispatchEvent(new HashChangeEvent("hashchange", { oldURL, newURL: location.href })));
}
beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});
afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.restoreAllMocks();
});

it.each([
  "appearance", "terminal-theme", "notifications", "password",
  "projects", "connections", "models", "instructions",
  "workers", "mobile", "daemon",
])("opens the %s page at its canonical address and leaves the address alone", (section) => {
  const replace = vi.spyOn(history, "replaceState");
  mount(`/settings/${section}`);
  replace.mockClear();
  change(`/settings/${section}`);
  expect(shown()).toEqual({ name: "settings", section });
  expect(location.hash).toBe(`#/settings/${section}`);
  expect(replace).not.toHaveBeenCalled();
});

it.each([
  ["/settings", "/settings/appearance"],
  ["/settings/unknown", "/settings/appearance"],
  ["/manage", "/settings/projects"],
  ["/manage/unknown", "/settings/projects"],
  ["/manage/projects", "/settings/projects"],
  ["/manage/connections", "/settings/connections"],
  ["/manage/models", "/settings/models"],
  ["/manage/instructions", "/settings/instructions"],
  ["/manage/workers", "/settings/workers"],
  ["/manage/mobile", "/settings/mobile"],
  ["/manage/daemon", "/settings/daemon"],
  ["/manage/projects?bucket=3&project=12", "/settings/projects?bucket=3&project=12"],
  ["/manage/projects?catalog=buckets", "/settings/projects?catalog=buckets"],
])("redirects the old address %s to %s on a direct load", (old, canonical) => {
  mount(old);
  expect(location.hash).toBe(`#${canonical}`);
  expect(shown()).toMatchObject({ name: "settings" });
});

it("redirects an old address reached by navigation without adding a history entry", () => {
  mount("/session/42");
  const push = vi.spyOn(history, "pushState");
  const before = history.length;
  location.hash = "#/manage/workers";
  act(() => window.dispatchEvent(new HashChangeEvent("hashchange")));
  expect(location.hash).toBe("#/settings/workers");
  expect(shown()).toEqual({ name: "settings", section: "workers" });
  expect(push).not.toHaveBeenCalled();
  expect(history.length).toBe(before + 1);
});

it("follows the address back out of Settings to the session it came from", () => {
  mount("/session/42?tab=terminal%3A7&focus=1");
  const session = shown();
  change("/settings/daemon");
  expect(shown()).toEqual({ name: "settings", section: "daemon" });
  change("/session/42?tab=terminal%3A7&focus=1");
  expect(shown()).toEqual(session);
});
