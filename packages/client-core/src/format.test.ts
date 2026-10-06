import { create } from "@bufbuild/protobuf";
import { describe, expect, it } from "vitest";
import { SessionSchema, SessionState } from "./gen/pm/v1/pm_pb";
import {
  formatAgo,
  sessionDisplayName,
  sessionLastActive,
  sessionLastActivity,
  sessionLastActivityTitle,
  sessionMatchesSearch,
  sessionStatusLine,
  stateStyle,
  unescapeHtml,
} from "./format";

describe("formatAgo", () => {
  it("collapses to a single coarse unit", () => {
    expect(formatAgo(10_000)).toBe("10s");
    expect(formatAgo(5 * 60_000)).toBe("5m");
    expect(formatAgo(3 * 3_600_000)).toBe("3h");
    expect(formatAgo(2 * 86_400_000)).toBe("2d");
    expect(formatAgo(-5_000)).toBe("0s");
  });
});

describe("sessionLastActive", () => {
  it("reports how long since the last activity", () => {
    const s = create(SessionSchema, { lastActivityAtUnixMs: 100_000n });
    expect(sessionLastActive(s, 110_000)).toBe("10s ago");
  });

  it("ignores the raw output and typing clocks the daemon keeps for itself", () => {
    const s = create(SessionSchema, {
      lastActivityAtUnixMs: 100_000n,
      lastAgentActivityAtUnixMs: 104_000n,
      lastUserInteractionAtUnixMs: 108_000n,
    });
    expect(sessionLastActive(s, 110_000)).toBe("10s ago");
  });

  it("is blank when there is no activity stamp", () => {
    const s = create(SessionSchema, { lastActivityAtUnixMs: 0n });
    expect(sessionLastActive(s, 110_000)).toBe("");
  });
});

describe("sessionLastActivity", () => {
  it("uses honest compact thresholds", () => {
    const now = 4_000_000;
    expect(sessionLastActivity(create(SessionSchema, {
      lastActivityAtUnixMs: BigInt(now - 9_000),
    }), now)).toBe("now");
    expect(sessionLastActivity(create(SessionSchema, {
      lastActivityAtUnixMs: BigInt(now - 38_000),
    }), now)).toBe("38s");
    expect(sessionLastActivity(create(SessionSchema, {
      lastActivityAtUnixMs: BigInt(now - 6 * 60_000),
    }), now)).toBe("6m");
    expect(sessionLastActivity(create(SessionSchema, {
      lastActivityAtUnixMs: BigInt(now - 2 * 3_600_000),
    }), now)).toBe("2h");
  });

  it("does not substitute the raw output or typing clocks for a missing activity clock", () => {
    const session = create(SessionSchema, {
      lastAgentActivityAtUnixMs: 100_000n,
      lastUserInteractionAtUnixMs: 100_000n,
    });
    expect(sessionLastActivity(session, 110_000)).toBe("unknown");
    expect(sessionLastActivityTitle(session, 110_000)).toBe("Last activity unknown");
  });

  it("labels ended sessions inactive instead of showing a stale age", () => {
    const session = create(SessionSchema, {
      state: SessionState.EXITED,
      lastActivityAtUnixMs: 100_000n,
    });
    expect(sessionLastActivity(session, 110_000)).toBe("inactive");
    expect(sessionLastActivityTitle(session, 110_000)).toBe("Session inactive");
  });
});

describe("sessionDisplayName", () => {
  it("names the session by its goal before the title or headline", () => {
    expect(sessionDisplayName(create(SessionSchema, {
      id: 7n,
      goal: "Optimizing femtocell software",
      taskTitle: "initial title",
      headline: "Editing femtocell.rs 2/3",
    }))).toBe("Optimizing femtocell software");
    expect(sessionDisplayName(create(SessionSchema, {
      id: 7n,
      taskTitle: "initial title",
      headline: "active work",
    }))).toBe("initial title");
    expect(sessionDisplayName(create(SessionSchema, {
      id: 7n,
      headline: "active work",
    }))).toBe("active work");
    expect(sessionDisplayName(create(SessionSchema, { id: 7n }))).toBe("session 7");
  });
});

describe("sessionStatusLine", () => {
  it("returns the headline under the goal", () => {
    expect(sessionStatusLine(create(SessionSchema, {
      goal: "Optimizing femtocell software",
      headline: "Editing femtocell.rs 2/3",
    }))).toBe("Editing femtocell.rs 2/3");
  });

  it("is empty when the name already shows the headline or there is none", () => {
    expect(sessionStatusLine(create(SessionSchema, { headline: "active work" }))).toBe("");
    expect(sessionStatusLine(create(SessionSchema, { goal: "Moving auth" }))).toBe("");
  });
});

describe("sessionMatchesSearch", () => {
  const session = create(SessionSchema, {
    goal: "Optimizing femtocell software",
    taskTitle: "item 12",
    headline: "Editing scheduler.rs",
  });

  it("matches the goal and the headline case-insensitively", () => {
    expect(sessionMatchesSearch(session, "FEMTOCELL")).toBe(true);
    expect(sessionMatchesSearch(session, "scheduler")).toBe(true);
    expect(sessionMatchesSearch(session, "frontend")).toBe(false);
  });

  it("matches everything for a blank query", () => {
    expect(sessionMatchesSearch(session, "  ")).toBe(true);
  });
});

describe("stateStyle", () => {
  it("presents offline recovery as awaiting a worker", () => {
    expect(stateStyle(SessionState.AWAITING_WORKER)).toEqual({
      label: "awaiting worker",
      className: "st-awaiting-worker",
    });
  });
});

describe("unescapeHtml", () => {
  it("decodes named entities", () => {
    expect(unescapeHtml("Building &amp; testing")).toBe("Building & testing");
    expect(unescapeHtml("&lt;div&gt; &amp; &quot;quote&quot; &apos;done&apos;")).toBe(
      "<div> & \"quote\" 'done'",
    );
    expect(unescapeHtml("step 1&nbsp;&mdash;&nbsp;done")).toBe("step 1 — done");
  });

  it("decodes decimal and hex numeric entities", () => {
    expect(unescapeHtml("&#38; &#39; &#34; &#60; &#62;")).toBe("& ' \" < >");
    expect(unescapeHtml("&#x26; &#x27; &#x22; &#x3c; &#x3e;")).toBe("& ' \" < >");
  });

  it("leaves strings without entities alone", () => {
    expect(unescapeHtml("plain text")).toBe("plain text");
    expect(unescapeHtml("AT&T and 5 < 10")).toBe("AT&T and 5 < 10");
  });

  it("unescapes display name and status line with entities", () => {
    const s = create(SessionSchema, {
      goal: "Auth &amp; Permissions",
      headline: "Fixing &lt;div&gt; &amp; &quot;styles&quot;",
    });
    expect(sessionDisplayName(s)).toBe("Auth & Permissions");
    expect(sessionStatusLine(s)).toBe("Fixing <div> & \"styles\"");
  });
});
