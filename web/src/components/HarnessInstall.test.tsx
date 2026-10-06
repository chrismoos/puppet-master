import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { HarnessInstall } from "./HarnessInstall";

describe("harness installation display", () => {
  it("asks for confirmation by harness name", () => {
    const html = renderToStaticMarkup(<HarnessInstall checking={false} confirm={() => {}} reply={{ agent: "claude", status: { state: "missing", command: "installer", output: "", error: "" } }} />);
    expect(html).toContain("Claude Code is not installed on this worker. Install it?");
    expect(html).toContain(">Install</button>");
    expect(html).toContain(">Cancel</button>");
  });
  it("shows activity and safely renders installer errors", () => {
    const html = renderToStaticMarkup(<HarnessInstall checking={false} reply={{ agent: "codex", status: { state: "installing", command: "installer", output: "<script>failed</script>", error: "permission denied" } }} />);
    expect(html).toContain('aria-label="Installing Codex"');
    expect(html).toContain('aria-label="Installer output"');
    expect(html).toContain("permission denied");
    expect(html).toContain("&lt;script&gt;");
    expect(html).not.toContain("<script>");
  });
});
