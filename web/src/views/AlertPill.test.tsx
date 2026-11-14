import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { AlertPill, pageIsWatched, type InAppAlert } from "./AlertPill";

const docAt = (visibilityState: DocumentVisibilityState, focused: boolean) =>
  ({ visibilityState, hasFocus: () => focused }) as unknown as Document;

describe("pageIsWatched", () => {
  it("is true only for a visible page in a focused window", () => {
    expect(pageIsWatched(docAt("visible", true))).toBe(true);
    expect(pageIsWatched(docAt("visible", false))).toBe(false);
    expect(pageIsWatched(docAt("hidden", true))).toBe(false);
  });
});

describe("AlertPill", () => {
  const question: InAppAlert = { key: "needs-input:4", kind: "needs-input", title: "Codex audit", body: "Type into your shells?", sessionId: "4" };
  const finished: InAppAlert = { key: "finished:7", kind: "finished", title: "Pane layout", sessionId: "7" };
  const render = (alerts: InAppAlert[]) => renderToStaticMarkup(<AlertPill alerts={alerts} onOpen={() => {}} onDismiss={() => {}} />);

  it("renders nothing without alerts", () => {
    expect(render([])).toBe("");
  });

  it("shows the newest alert with its kind and a count of the rest", () => {
    const html = render([question, finished]);
    expect(html).toContain("alert-pill-needs-input");
    expect(html).toContain(">needs you<");
    expect(html).toContain(">Codex audit<");
    expect(html).toContain(">Type into your shells?<");
    expect(html).toContain(">+1<");
  });

  it("leaves out the count when one alert is waiting", () => {
    const html = render([finished]);
    expect(html).toContain("alert-pill-finished");
    expect(html).not.toContain("alert-pill-more");
  });
});
