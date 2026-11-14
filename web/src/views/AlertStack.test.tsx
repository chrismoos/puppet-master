// @vitest-environment jsdom
import { act, useState } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, describe, expect, it, vi } from "vitest";
import { create } from "@bufbuild/protobuf";
import { AgentNotificationLinkKind, AgentNotificationLinkSchema } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import {
  AlertStack,
  AUTO_HIDE_MS,
  alertLinkFrom,
  pageIsWatched,
  relativeAge,
  type InAppAlert,
} from "./AlertStack";

const NOW = Date.UTC(2026, 10, 14, 12, 0, 0);
const MINUTE_MS = 60_000;

const docAt = (visibilityState: DocumentVisibilityState, focused: boolean) =>
  ({ visibilityState, hasFocus: () => focused }) as unknown as Document;

function alert(key: string, kind: InAppAlert["kind"], extra: Partial<InAppAlert> = {}): InAppAlert {
  return { key, kind, title: `session ${key}`, raisedAtMs: NOW, ...extra };
}

let host: HTMLDivElement;
let root: Root;
const opened: string[] = [];
const linked: string[] = [];

/** Holds the list the way App does, so dismissing and expiring really remove cards. */
function Harness({ initial }: { initial: InAppAlert[] }) {
  const [alerts, setAlerts] = useState(initial);
  return (
    <AlertStack
      alerts={alerts}
      now={() => NOW}
      onOpen={(a) => opened.push(a.key)}
      onOpenLink={(a) => linked.push(a.key)}
      onDismiss={(key) => setAlerts((current) => current.filter((a) => a.key !== key))}
      onDismissAll={() => setAlerts([])}
    />
  );
}

function render(initial: InAppAlert[]) {
  act(() => root.render(<Harness initial={initial} />));
}

const cards = () => [...host.querySelectorAll<HTMLElement>(".alert-card")];
const titles = () => cards().map((card) => card.querySelector(".alert-card-title")?.textContent);
const stack = () => host.querySelector<HTMLElement>(".alert-stack");

function hover(element: Element, over: boolean) {
  // React listens for mouseover/mouseout to synthesize enter and leave.
  act(() => {
    element.dispatchEvent(new MouseEvent(over ? "mouseover" : "mouseout", { bubbles: true, relatedTarget: document.body }));
  });
}

beforeEach(() => {
  Object.assign(globalThis, { IS_REACT_ACT_ENVIRONMENT: true });
  vi.useFakeTimers({ now: NOW });
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
  opened.length = 0;
  linked.length = 0;
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
  vi.useRealTimers();
});

describe("pageIsWatched", () => {
  it("is true only for a visible page in a focused window", () => {
    expect(pageIsWatched(docAt("visible", true))).toBe(true);
    expect(pageIsWatched(docAt("visible", false))).toBe(false);
    expect(pageIsWatched(docAt("hidden", true))).toBe(false);
  });
});

describe("relativeAge", () => {
  it("reads as just now under a minute, then minutes, then hours", () => {
    expect(relativeAge(NOW, NOW + 30_000)).toBe("just now");
    expect(relativeAge(NOW, NOW + 2 * MINUTE_MS)).toBe("2m ago");
    expect(relativeAge(NOW, NOW + 125 * MINUTE_MS)).toBe("2h ago");
  });
});

describe("alertLinkFrom", () => {
  it("maps each link kind and drops a link with no URL", () => {
    const page = create(AgentNotificationLinkSchema, { kind: AgentNotificationLinkKind.PAGE, title: "report", url: "https://r.example/" });
    expect(alertLinkFrom(page)).toEqual({ kind: "page", title: "report", url: "https://r.example/" });
    const item = create(AgentNotificationLinkSchema, { kind: AgentNotificationLinkKind.ITEM, title: "Fix it", url: "/bucket/1/item/4" });
    expect(alertLinkFrom(item)?.kind).toBe("item");
    expect(alertLinkFrom(create(AgentNotificationLinkSchema, { kind: AgentNotificationLinkKind.PAGE }))).toBeUndefined();
    expect(alertLinkFrom(undefined)).toBeUndefined();
  });
});

describe("AlertStack", () => {
  it("renders nothing without alerts", () => {
    render([]);
    expect(stack()).toBeNull();
  });

  it("shows a card's kind, session, detail line, body and age", () => {
    render([alert("4", "needs-input", { title: "Codex audit", detail: "Auditing the shell tools", body: "Type into your shells?" })]);
    const card = cards()[0];
    expect(card.classList).toContain("alert-card-needs-input");
    expect(card.getAttribute("aria-label")).toBe("needs you: Codex audit");
    expect(card.querySelector(".alert-card-dot")?.getAttribute("title")).toBe("needs you");
    expect(card.querySelector(".alert-card-title")?.textContent).toBe("Codex audit");
    expect(card.querySelector(".alert-card-detail")?.textContent).toBe("Auditing the shell tools");
    expect(card.querySelector(".alert-card-body")?.textContent).toBe("Type into your shells?");
    expect(card.querySelector(".alert-card-age")?.textContent).toBe("just now");
  });

  it("stacks the newest three, compacts the older ones, and counts the rest", () => {
    render(["1", "2", "3", "4", "5"].map((key) => alert(key, "finished")));
    expect(titles()).toEqual(["session 1", "session 2", "session 3"]);
    expect(cards().map((card) => card.classList.contains("is-compact"))).toEqual([false, true, true]);
    expect(host.querySelector(".alert-stack-more")?.textContent).toBe("+2 more");
  });

  it("offers Dismiss all only once two cards are up", () => {
    render([alert("1", "failed")]);
    expect(host.querySelector(".alert-stack-clear")).toBeNull();
    act(() => root.unmount());
    root = createRoot(host);
    render([alert("1", "failed"), alert("2", "finished")]);
    act(() => host.querySelector<HTMLButtonElement>(".alert-stack-clear")!.click());
    expect(stack()).toBeNull();
  });

  it("opens the subject from a click on the card, while × only dismisses that card", () => {
    render([alert("1", "needs-input"), alert("2", "finished")]);
    act(() => cards()[1].querySelector<HTMLButtonElement>(".alert-card-open")!.click());
    expect(opened).toEqual(["2"]);
    act(() => cards()[0].querySelector<HTMLButtonElement>(".alert-card-close")!.click());
    expect(opened).toEqual(["2"]);
    expect(titles()).toEqual(["session 2"]);
  });

  it("removes each card on its own when its time is up", () => {
    render([alert("q", "needs-input"), alert("f", "finished")]);
    act(() => vi.advanceTimersByTime(AUTO_HIDE_MS.finished));
    expect(titles()).toEqual(["session q"]);
    act(() => vi.advanceTimersByTime(AUTO_HIDE_MS["needs-input"] - AUTO_HIDE_MS.finished));
    expect(stack()).toBeNull();
  });

  it("holds every countdown while the pointer rests on the stack, and resumes with the time that was left", () => {
    render([alert("f", "finished")]);
    act(() => vi.advanceTimersByTime(AUTO_HIDE_MS.finished - 1_000));
    hover(stack()!, true);
    act(() => vi.advanceTimersByTime(60_000));
    expect(titles()).toEqual(["session f"]);
    expect(cards()[0].querySelector<HTMLElement>(".alert-card-timer")!.style.animationPlayState).toBe("paused");
    hover(stack()!, false);
    act(() => vi.advanceTimersByTime(999));
    expect(titles()).toEqual(["session f"]);
    act(() => vi.advanceTimersByTime(1));
    expect(stack()).toBeNull();
  });

  it("renders a message's title in bold before its body, and its link as its own target", () => {
    render([
      alert("m", "message", {
        heading: "Deploy finished",
        body: "staging is green",
        link: { kind: "page", title: "e2e timing report", url: "https://report.example/" },
      }),
    ]);
    const card = cards()[0];
    expect(card.querySelector(".alert-card-dot")?.getAttribute("title")).toBe("message");
    expect(card.querySelector(".alert-card-body strong")?.textContent).toBe("Deploy finished");
    expect(card.querySelector(".alert-card-body")?.textContent).toBe("Deploy finished staging is green");
    const link = card.querySelector<HTMLAnchorElement>(".alert-card-link")!;
    expect(link.getAttribute("href")).toBe("https://report.example/");
    expect(link.target).toBe("_blank");
    expect(link.querySelector(".alert-card-link-kind")?.textContent).toBe("published page");
    link.addEventListener("click", (event) => event.preventDefault());
    act(() => link.click());
    expect(linked).toEqual(["m"]);
    expect(opened).toEqual([]);
  });

  it("links a board item inside the dashboard rather than in a new tab", () => {
    render([alert("m", "message", { heading: "Filed", link: { kind: "item", title: "Fix login", url: "/bucket/1/item/42" } })]);
    const link = host.querySelector<HTMLAnchorElement>(".alert-card-link")!;
    expect(link.getAttribute("href")).toBe("#/bucket/1/item/42");
    expect(link.hasAttribute("target")).toBe(false);
  });
});
