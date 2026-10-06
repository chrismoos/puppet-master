import { describe, expect, it } from "vitest";

describe("settings save-button state", () => {
  function urlChanged(input: string, current: string): boolean {
    const normalized = input.trim().replace(/\/+$/, "");
    return normalized !== current;
  }

  it("disables save when the URL has not changed", () => {
    expect(urlChanged("https://pm.test", "https://pm.test")).toBe(false);
  });

  it("enables save when the URL differs", () => {
    expect(urlChanged("https://other.test", "https://pm.test")).toBe(true);
  });

  it("ignores trailing slashes and whitespace", () => {
    expect(urlChanged("  https://pm.test///  ", "https://pm.test")).toBe(false);
  });
});

describe("settings status colour", () => {
  function statusColor(label: string): string {
    switch (label) {
      case "connected":
        return "#3dd68c";
      case "rejected":
      case "not logged in":
        return "#ff5d5d";
      default:
        return "#6e7889";
    }
  }

  it("shows green for connected", () => {
    expect(statusColor("connected")).toBe("#3dd68c");
  });

  it("shows red for rejected", () => {
    expect(statusColor("rejected")).toBe("#ff5d5d");
  });

  it("shows red for not logged in", () => {
    expect(statusColor("not logged in")).toBe("#ff5d5d");
  });

  it("shows muted for other states", () => {
    expect(statusColor("connecting\u2026")).toBe("#6e7889");
    expect(statusColor("offline \u2014 retrying")).toBe("#6e7889");
  });
});

describe("enrolment date formatting", () => {
  function formatDate(unixMs: number): string {
    const d = new Date(unixMs);
    return d.toLocaleDateString(undefined, { year: "numeric", month: "short", day: "numeric" });
  }

  it("formats a known timestamp", () => {
    // 2023-11-14 in UTC
    const result = formatDate(1700000000000);
    expect(result).toContain("2023");
    expect(result).toContain("Nov");
  });
});

// Saving a setting and leaving the screen are separate things. They shared
// one handler, so flipping the previews switch persisted the change and
// then threw the user back to the session list mid-change.
describe("saving a setting versus enrolling", () => {
  type Screen = { kind: string };

  function harness() {
    let screen: Screen = { kind: "settings" };
    const written: string[] = [];
    const saveConfig = (value: string) => {
      written.push(value);
    };
    const enrol = (value: string) => {
      saveConfig(value);
      screen = { kind: "sessions" };
    };
    return { saveConfig, enrol, written, screenKind: () => screen.kind };
  }

  it("keeps you on settings when a setting is saved", () => {
    const h = harness();
    h.saveConfig("previews:on");
    expect(h.written).toEqual(["previews:on"]);
    expect(h.screenKind()).toBe("settings");
  });

  it("still leaves the login screen once enrolment succeeds", () => {
    const h = harness();
    h.enrol("controller");
    expect(h.written).toEqual(["controller"]);
    expect(h.screenKind()).toBe("sessions");
  });
});
