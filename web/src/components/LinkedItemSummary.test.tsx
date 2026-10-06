import { create } from "@bufbuild/protobuf";
import type { ReactElement } from "react";
import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it, vi } from "vitest";
import { ItemSchema, ItemStatus } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { LinkedItemSummary } from "./LinkedItemSummary";

function summary() {
  return {
    primary: create(ItemSchema, {
      id: 900719925474099312345n,
      bucketId: 1n,
      title: "A title that can use the available row width",
      status: ItemStatus.IN_PROGRESS,
    }),
    remaining: [
      create(ItemSchema, { id: 8n, bucketId: 1n, title: "Waiting for review", status: ItemStatus.BLOCKED_EXTERNAL }),
      create(ItemSchema, { id: 7n, bucketId: 1n, title: "Older work", status: ItemStatus.DONE }),
    ],
  };
}

function activationEvent(key?: string) {
  return { key, preventDefault: vi.fn(), stopPropagation: vi.fn() };
}

describe("LinkedItemSummary", () => {
  it("renders a compact lossless internal link with status and remainder details", () => {
    const markup = renderToStaticMarkup(
      <LinkedItemSummary summary={summary()} sessionId="53" onPmLink={() => {}} />,
    );

    expect(markup).toContain('href="#/bucket/1/item/900719925474099312345"');
    expect(markup).toContain("#900719925474099312345");
    expect(markup).toContain("in progress");
    expect(markup).toContain("+2");
    expect(markup).toContain("#8 · Waiting for review — waiting");
    expect(markup).toContain('tabindex="0"');
    expect(markup).not.toContain("target=");
    expect(markup).not.toContain("pm:item/");
  });

  it("can omit the visible status without losing it from the accessible description", () => {
    const markup = renderToStaticMarkup(
      <LinkedItemSummary
        summary={summary()}
        sessionId="53"
        onPmLink={() => {}}
        showStatus={false}
      />,
    );

    expect(markup).not.toContain("sb-linked-item-status");
    expect(markup).toContain("— in progress");
    expect(markup).toContain("A title that can use the available row width");
  });

  it.each([undefined, "Enter", " "])("routes %s activation without selecting the session", (key) => {
    const onPmLink = vi.fn();
    const row = LinkedItemSummary({ summary: summary(), sessionId: "53", onPmLink }) as ReactElement<{
      children: ReactElement<{ onClick: (event: ReturnType<typeof activationEvent>) => void; onKeyDown: (event: ReturnType<typeof activationEvent>) => void }>[];
    }>;
    const link = row.props.children[0];
    const event = activationEvent(key);

    if (key === undefined) link.props.onClick(event);
    else link.props.onKeyDown(event);

    expect(event.preventDefault).toHaveBeenCalledOnce();
    expect(event.stopPropagation).toHaveBeenCalledOnce();
    expect(onPmLink).toHaveBeenCalledWith(
      { kind: "item", bucketId: "1", id: "900719925474099312345" },
      "53",
    );
  });
});
