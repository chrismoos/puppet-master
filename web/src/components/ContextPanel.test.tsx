import { create } from "@bufbuild/protobuf";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import {
  ContextFieldSchema,
  ContextKind,
  SessionContextSchema,
} from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { ContextPanel } from "./ContextPanel";

function field(key: string, value: string) {
  return create(ContextFieldSchema, { key, label: key, value, kind: ContextKind.TEXT });
}

function render(detail: ReturnType<typeof field>[], activity = "") {
  return renderToStaticMarkup(
    <ContextPanel
      context={create(SessionContextSchema, { detail })}
      sessionId="7"
      changeToken="fixture"
      summary=""
      activity={activity}
    />,
  );
}

describe("ContextPanel activity", () => {
  it("renders the first-class session activity as a full-width row", () => {
    for (const value of ["Running tests", "Reviewing the browser layout before updating focused coverage across several viewport sizes."]) {
      const html = render([field("branch", "codex/layout")], value);
      expect(html).toContain('class="ctx-grid-row"');
      expect(html).toContain('class="ctx-grid-row is-activity"');
      expect(html).toContain(value);
    }
  });

  it("omits empty session activity without hiding context fields", () => {
    const html = render([field("branch", "main")], "  \n ");
    expect(html).toContain("branch");
    expect(html).not.toContain("is-activity");
  });

  it("does not mistake an arbitrary context key for the session activity", () => {
    const html = render([field("activity", "context-owned value")]);
    expect(html).toContain("context-owned value");
    expect(html).not.toContain("is-activity");
  });
});

describe("a URL context field", () => {
  const urlField = (value: string) =>
    create(ContextFieldSchema, { key: "preview", label: "preview", value, kind: ContextKind.URL });

  const renderUrl = (value: string) =>
    renderToStaticMarkup(
      <ContextPanel
        context={create(SessionContextSchema, { detail: [urlField(value)] })}
        sessionId="7"
        changeToken="fixture"
        summary=""
        activity=""
      />,
    );

  /// The value is written by an agent and entity-decoded on the way in, so
  /// nothing upstream promises it is a scheme a browser should follow. The
  /// glance chip has always classified it; this panel did not, which left
  /// React's own javascript:-specific sanitizing as the only thing in the way,
  /// and that covers one scheme rather than the question.
  it("is not an href unless it is a web URL", () => {
    for (const value of [
      "javascript:alert(1)",
      "data:text/html,<script>alert(1)</script>",
      "vbscript:msgbox",
      "file:///etc/passwd",
      "shortcuts://run-shortcut?name=wipe",
      "not-a-url",
    ]) {
      const html = renderUrl(value);
      expect(html, value).not.toContain("href=");
      // Shown inert rather than dropped: hiding the field would hide what the
      // agent reported, which is not the same problem.
      expect(html, value).toContain('class="ctx-grid-value"');
    }
  });

  it("is an href when it is one", () => {
    expect(renderUrl("https://example.com/report")).toContain(
      'href="https://example.com/report"',
    );
  });

  /// A pm: link is navigable inside the app, not out of it.
  it("does not become an external href for a pm link", () => {
    expect(renderUrl("pm:item/1/2")).not.toContain("href=");
  });
});
