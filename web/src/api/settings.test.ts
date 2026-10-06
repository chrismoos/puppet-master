import { describe, expect, it } from "vitest";
import {
  isBooleanSetting,
  isNumericSetting,
  settingMetadata,
  type DaemonSetting,
} from "./settings";

describe("settings predicate helpers", () => {
  it("identifies boolean settings from their default value", () => {
    expect(isBooleanSetting({ default: "true" } as DaemonSetting)).toBe(true);
    expect(isBooleanSetting({ default: "false" } as DaemonSetting)).toBe(true);
    expect(isBooleanSetting({ default: "8" } as DaemonSetting)).toBe(false);
    expect(isBooleanSetting({ default: "abc" } as DaemonSetting)).toBe(false);
  });

  it("identifies numeric settings from their default value", () => {
    expect(isNumericSetting({ default: "8" } as DaemonSetting)).toBe(true);
    expect(isNumericSetting({ default: "15" } as DaemonSetting)).toBe(true);
    expect(isNumericSetting({ default: "true" } as DaemonSetting)).toBe(false);
    expect(isNumericSetting({ default: "false" } as DaemonSetting)).toBe(false);
    expect(isNumericSetting({ default: "abc" } as DaemonSetting)).toBe(false);
  });
});

describe("setting metadata resolution", () => {
  it("resolves known spawn settings to Agent terminal", () => {
    const fullscreen = settingMetadata("spawn.fullscreen");
    expect(fullscreen.title).toBe("Fullscreen alternate screen for Claude Code");
    expect(fullscreen.category).toBe("Agent terminal");

    const truecolor = settingMetadata("spawn.truecolor");
    expect(truecolor.title).toBe("24-bit truecolor");
    expect(truecolor.category).toBe("Agent terminal");
  });

  it("resolves known supervisor settings to Supervisors", () => {
    const supervisor = settingMetadata("supervisor.max_children");
    expect(supervisor.title).toBe("Concurrent children per supervisor");
    expect(supervisor.category).toBe("Supervisors");
    expect(supervisor.unit).toBe("sessions");
    expect(supervisor.min).toBe(1);
    expect(supervisor.step).toBe(1);
  });

  it("resolves known mobile and push settings to Mobile app", () => {
    const mobileTtl = settingMetadata("mobile.access_token_ttl_minutes");
    expect(mobileTtl.title).toBe("Access token lifetime");
    expect(mobileTtl.category).toBe("Mobile app");
    expect(mobileTtl.unit).toBe("minutes");
    expect(mobileTtl.step).toBe(5);

    const dedupe = settingMetadata("push.dedupe_window_hours");
    expect(dedupe.title).toBe("Push deduplication window");
    expect(dedupe.category).toBe("Mobile app");
    expect(dedupe.unit).toBe("hours");
  });

  it("infers fallback category and unit for unknown settings", () => {
    const customSpawn = settingMetadata("spawn.pty_buffer_seconds");
    expect(customSpawn.category).toBe("Agent terminal");
    expect(customSpawn.unit).toBe("seconds");
    expect(customSpawn.title).toBe("Pty buffer seconds");

    const customGeneral = settingMetadata("custom_timeout_minutes");
    expect(customGeneral.category).toBe("General");
    expect(customGeneral.unit).toBe("minutes");
    expect(customGeneral.title).toBe("Custom timeout minutes");
  });
});
