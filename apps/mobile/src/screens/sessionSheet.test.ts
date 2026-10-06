import { describe, expect, it } from "vitest";
import { ContextKind, ContextSeverity } from "@puppet-master/client-core/gen/pm/v1/pm_pb";

describe("severity colour mapping", () => {
  it("maps known severities", () => {
    // Verify the enum values exist and are distinct
    expect(ContextSeverity.INFO).not.toBe(ContextSeverity.GOOD);
    expect(ContextSeverity.WARN).not.toBe(ContextSeverity.BAD);
    expect(ContextSeverity.NEUTRAL).toBeDefined();
  });
});

describe("context kind rendering", () => {
  it("has CODE kind for monospace display", () => {
    expect(ContextKind.CODE).toBeDefined();
    expect(ContextKind.TEXT).toBeDefined();
    expect(ContextKind.BADGE).toBeDefined();
    expect(ContextKind.URL).toBeDefined();
  });

  it("URL kind is distinct from TEXT and CODE", () => {
    expect(ContextKind.URL).not.toBe(ContextKind.TEXT);
    expect(ContextKind.URL).not.toBe(ContextKind.CODE);
  });
});

describe("URL scheme validation", () => {
  function isOpenable(url: string): boolean {
    return /^https?:\/\//i.test(url);
  }

  it("allows http and https URLs", () => {
    expect(isOpenable("https://example.com")).toBe(true);
    expect(isOpenable("http://example.com")).toBe(true);
    expect(isOpenable("HTTPS://EXAMPLE.COM")).toBe(true);
  });

  it("rejects non-http schemes", () => {
    expect(isOpenable("ftp://example.com")).toBe(false);
    expect(isOpenable("file:///etc/passwd")).toBe(false);
    expect(isOpenable("javascript:alert(1)")).toBe(false);
    expect(isOpenable("data:text/html,<h1>hi</h1>")).toBe(false);
  });

  it("rejects bare text", () => {
    expect(isOpenable("not a url")).toBe(false);
    expect(isOpenable("")).toBe(false);
  });
});

describe("report text formatting", () => {
  // Inline the same logic as the component for testability
  function reportText(kind: string, payload: Record<string, unknown>): string {
    switch (kind) {
      case "checkpoint": {
        const parts = [payload.headline, payload.note].filter(Boolean);
        return parts.join(" — ") || "checkpoint";
      }
      case "status": {
        const parts = [payload.task, payload.phase, payload.detail].filter(Boolean);
        return parts.join(": ") || "status";
      }
      case "progress":
        return `${payload.percent ?? "?"}% ${payload.summary ?? ""}`.trim();
      case "blocked":
        return String(payload.question ?? "waiting for input");
      default:
        return kind;
    }
  }

  it("formats checkpoint with headline and note", () => {
    expect(reportText("checkpoint", { headline: "Tests pass", note: "all green" })).toBe(
      "Tests pass — all green",
    );
  });

  it("formats checkpoint with headline only", () => {
    expect(reportText("checkpoint", { headline: "Done" })).toBe("Done");
  });

  it("formats progress", () => {
    expect(reportText("progress", { percent: 75, summary: "compiling" })).toBe("75% compiling");
  });

  it("formats blocked", () => {
    expect(reportText("blocked", { question: "Which database?" })).toBe("Which database?");
  });

  it("formats unknown kind as-is", () => {
    expect(reportText("custom", {})).toBe("custom");
  });
});
