import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { SessionKillDialog } from "./SessionKillDialog";

describe("session kill dialog", () => {
  it("names the session and explains the destructive action", () => {
    const markup = renderToStaticMarkup(createElement(SessionKillDialog, {
      sessionName: "terminal theme worker",
      onClose: () => {},
      onConfirm: async () => {},
    }));

    expect(markup).toContain('role="dialog"');
    expect(markup).toContain("Kill “terminal theme worker”?");
    expect(markup).toContain("stops the agent process and ends the session");
    expect(markup).toContain("kill session</button>");
  });
});
