import { create } from "@bufbuild/protobuf";
import type { ReactElement } from "react";
import { describe, expect, it, vi } from "vitest";
import {
  ContextFieldSchema,
  ContextKind,
  ContextSeverity,
} from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import { ContextChip, progressPercent, severityClass } from "./ContextChip";

describe("severityClass", () => {
  it("maps each severity to its class", () => {
    expect(severityClass(ContextSeverity.GOOD)).toBe("ctx-good");
    expect(severityClass(ContextSeverity.WARN)).toBe("ctx-warn");
    expect(severityClass(ContextSeverity.BAD)).toBe("ctx-bad");
    expect(severityClass(ContextSeverity.INFO)).toBe("ctx-info");
    expect(severityClass(ContextSeverity.NEUTRAL)).toBe("ctx-neutral");
  });

  it("falls back to neutral for unspecified", () => {
    expect(severityClass(ContextSeverity.UNSPECIFIED)).toBe("ctx-neutral");
  });
});

describe("progressPercent", () => {
  it("clamps numeric values into 0-100", () => {
    expect(progressPercent("42")).toBe(42);
    expect(progressPercent("42.5")).toBe(42.5);
    expect(progressPercent("-5")).toBe(0);
    expect(progressPercent("130")).toBe(100);
  });

  it("returns null for non-numeric values", () => {
    expect(progressPercent("n/a")).toBeNull();
    expect(progressPercent("")).toBeNull();
  });
});

interface ChipProps {
  href?: string;
  target?: string;
  rel?: string;
  onClick?: (event: ReturnType<typeof activationEvent>) => void;
  onKeyDown?: (event: ReturnType<typeof activationEvent>) => void;
}

function urlChip(value: string, onPmLink = vi.fn()): ReactElement<ChipProps> {
  return ContextChip({
    field: create(ContextFieldSchema, {
      key: "work_item",
      label: "Item",
      kind: ContextKind.URL,
      value,
    }),
    onPmLink,
  }) as ReactElement<ChipProps>;
}

function activationEvent(key?: string) {
  return {
    key,
    preventDefault: vi.fn(),
    stopPropagation: vi.fn(),
  };
}

describe("PM URL chips", () => {
  it("renders a qualified item as a same-origin route without a new-tab target", () => {
    const chip = urlChip("pm:item/1/33");

    expect(chip.type).toBe("a");
    expect(chip.props.href).toBe("#/bucket/1/item/33");
    expect(chip.props.target).toBeUndefined();
    expect(chip.props.rel).toBeUndefined();
  });

  it("stops row propagation and routes pointer activation internally", () => {
    const onPmLink = vi.fn();
    const chip = urlChip("pm:item/1/33", onPmLink);
    const event = activationEvent();

    chip.props.onClick!(event);

    expect(event.preventDefault).toHaveBeenCalledOnce();
    expect(event.stopPropagation).toHaveBeenCalledOnce();
    expect(onPmLink).toHaveBeenCalledWith({ kind: "item", bucketId: "1", id: "33" });
  });

  it.each(["Enter", " "])(
    "routes %j keyboard activation without selecting the row",
    (key) => {
      const onPmLink = vi.fn();
      const chip = urlChip("pm:item/7/900719925474099312345", onPmLink);
      const event = activationEvent(key);

      chip.props.onKeyDown!(event);

      expect(event.preventDefault).toHaveBeenCalledOnce();
      expect(event.stopPropagation).toHaveBeenCalledOnce();
      expect(onPmLink).toHaveBeenCalledWith({
        kind: "item",
        bucketId: "7",
        id: "900719925474099312345",
      });
    },
  );

  it("keeps ordinary HTTPS links external and malformed PM values inert", () => {
    const external = urlChip("https://example.invalid/work/33");
    const malformed = urlChip("pm:item/not-a-number");

    expect(external.props.target).toBe("_blank");
    expect(external.props.rel).toBe("noopener noreferrer");
    expect(malformed.type).toBe("span");
  });
});
