import { renderToStaticMarkup } from "react-dom/server";
import { describe, expect, it } from "vitest";
import { DaemonSettingsPanel } from "./DaemonSettingsPanel";
import type { DaemonSetting } from "../api/settings";

describe("DaemonSettingsPanel", () => {
  const sampleSettings: DaemonSetting[] = [
    {
      key: "spawn.fullscreen",
      value: "false",
      default: "false",
      set: false,
      description: "let spawned agents take over the alternate screen",
    },
    {
      key: "spawn.truecolor",
      value: "true",
      default: "true",
      set: true,
      description: "advertise 24-bit color to spawned agents",
    },
    {
      key: "supervisor.max_children",
      value: "8",
      default: "8",
      set: false,
      description: "how many live sessions one supervisor session may have spawned",
    },
    {
      key: "mobile.access_token_ttl_minutes",
      value: "15",
      default: "15",
      set: false,
      description: "minutes a mobile bearer access token stays valid",
    },
  ];

  const html = renderToStaticMarkup(
    <DaemonSettingsPanel settings={sampleSettings} onWriteSetting={() => {}} />,
  );

  it("groups settings under one page title", () => {
    expect(html.match(/<h2/g)).toHaveLength(1);
    expect(html).toContain(">Daemon</h2>");
    expect(html).toContain("Agent terminal");
    expect(html).toContain("Supervisors");
    expect(html).toContain("Mobile app");
  });

  it("renders each setting as a row with its title and description", () => {
    expect(html.match(/class="ui-row set-setting/g)).toHaveLength(sampleSettings.length);
    expect(html).toContain("Fullscreen alternate screen for Claude Code");
    expect(html).toContain("24-bit truecolor");
    expect(html).toContain("Concurrent children per supervisor");
    expect(html).toContain("advertise 24-bit color to spawned agents");
  });

  it("marks only a changed setting, with its default and a reset", () => {
    expect(html.match(/set-setting modified/g)).toHaveLength(1);
    expect(html).toMatch(/set-setting modified" data-key="spawn\.truecolor"/);
    expect(html.match(/changed from true/g)).toHaveLength(1);
    expect(html.match(/aria-label="Reset /g)).toHaveLength(1);
    expect(html).toContain('aria-label="Reset 24-bit truecolor"');
  });

  it("renders switches for booleans and steppers for numbers", () => {
    expect(html.match(/class="ui-switch"/g)).toHaveLength(2);
    expect(html.match(/class="ui-stepper"/g)).toHaveLength(2);
    expect(html).toContain('aria-label="Decrease Concurrent children per supervisor"');
    expect(html).toContain('aria-label="Increase Access token lifetime"');
    expect(html).toContain("sessions");
    expect(html).toContain("minutes");
  });

  it("shows the config key as one line that copies the pm config set command", () => {
    expect(html).toContain('title="Copy: pm config set spawn.fullscreen false"');
    expect(html).toContain('title="Copy: pm config set supervisor.max_children 8"');
    expect(html).toContain(">spawn.fullscreen</button>");
  });
});
