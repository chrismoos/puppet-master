import { createElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { WorkspaceDeleteDialog } from "./WorkspaceDeleteDialog";

describe("workspace delete dialog", () => {
  it("names the workspace and explains what deletion preserves", () => {
    const markup = renderToStaticMarkup(createElement(WorkspaceDeleteDialog, {
      workspaceName: "release watch",
      onClose: () => {},
      onConfirm: async () => {},
    }));

    expect(markup).toContain('role="dialog"');
    expect(markup).toContain("Delete “release watch”?");
    expect(markup).toContain("Agent sessions and terminals will keep running.");
    expect(markup).toContain("delete workspace</button>");
  });
});
