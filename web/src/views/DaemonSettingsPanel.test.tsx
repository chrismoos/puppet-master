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

describe("voice settings", () => {
  const voiceSettings: DaemonSetting[] = [
    {
      key: "voice.tts.provider",
      value: "elevenlabs",
      default: "elevenlabs",
      set: false,
      options: ["elevenlabs"],
      description: "which text-to-speech provider renders the voice agent's answers",
    },
    {
      key: "voice.tts.voice",
      value: "",
      default: "",
      set: false,
      description: "the provider's own identifier for the voice to speak in",
    },
    {
      key: "voice.tts.api_key",
      value: "",
      default: "",
      set: true,
      secret: true,
      description: "the provider credential, sealed at rest and never read back",
    },
  ];

  const html = renderToStaticMarkup(
    <DaemonSettingsPanel settings={voiceSettings} onWriteSetting={() => {}} />,
  );

  it("groups them under Voice rather than leaving them loose in General", () => {
    expect(html).toContain("Voice");
    expect(html).toContain("Speech provider");
    expect(html).not.toContain("Tts.provider");
  });

  /// The provider is a known set, so it is chosen rather than typed: a
  /// mistyped provider name is a silent failure at the next answer.
  it("offers the providers the daemon reports as a choice", () => {
    expect(html).toContain("<select");
    expect(html).toContain('value="elevenlabs"');
  });

  /**
   * The daemon never returns a stored credential, so a text box would
   * show an empty value beside a key that is set and invite the user to
   * overwrite it with nothing. It is a password field reporting only
   * whether one is stored.
   */
  it("never renders a credential as readable text", () => {
    expect(html).toContain('type="password"');
    expect(html).toContain('placeholder="stored"');
    // The copyable command must not offer to echo a credential either.
    expect(html).toContain("pm config set voice.tts.api_key &lt;value&gt;");
  });
});

describe("a setting whose values are a known set", () => {
  const withProfiles: DaemonSetting[] = [
    {
      key: "voice.model.profile",
      value: "",
      default: "",
      set: false,
      options: ["openrouter", "work account"],
      description: "the model profile the voice agent reasons with",
    },
  ];

  /**
   * A select whose value is not among its options displays the first
   * one instead, which would show an unset setting as though a profile
   * were chosen. The stored value is therefore always offered.
   */
  it("shows an unset value as unset rather than as the first choice", () => {
    const html = renderToStaticMarkup(
      <DaemonSettingsPanel settings={withProfiles} onWriteSetting={() => {}} />,
    );
    expect(html).toContain("not set");
    expect(html).toContain('value="openrouter"');
    expect(html).toContain("work account");
  });

  /// A profile that has since been renamed or deleted is still what the
  /// setting says, so it is shown rather than quietly replaced.
  it("keeps showing a value that is no longer one of the choices", () => {
    const stale = [{ ...withProfiles[0], value: "deleted profile", set: true }];
    const html = renderToStaticMarkup(
      <DaemonSettingsPanel settings={stale} onWriteSetting={() => {}} />,
    );
    expect(html).toContain("deleted profile");
  });

  /// With no profiles at all a dropdown would be an empty box with no
  /// explanation, so it falls back to a field the key can be typed into.
  it("falls back to free text when there are no choices", () => {
    const none = [{ ...withProfiles[0], options: [] }];
    const html = renderToStaticMarkup(
      <DaemonSettingsPanel settings={none} onWriteSetting={() => {}} />,
    );
    expect(html).not.toContain("<select");
    expect(html).toContain('type="text"');
  });
});

describe("a setting whose choices come from a provider", () => {
  const modelSetting: DaemonSetting[] = [
    {
      key: "voice.tts.model",
      value: "eleven_flash_v2_5",
      default: "",
      set: true,
      choices_url: "/api/voice/speech-models",
      description: "which of the provider's models renders the speech",
    },
  ];

  /**
   * Listing costs a credentialed round trip to the provider, so it is
   * offered as a button rather than done on load: a settings page must
   * open without a speech credential, and offline.
   */
  it("offers to list them rather than fetching on load", () => {
    const html = renderToStaticMarkup(
      <DaemonSettingsPanel settings={modelSetting} onWriteSetting={() => {}} />,
    );
    expect(html).toContain("List choices for");
    // Still typable, because a model newer than the provider's own
    // listing has to stay selectable.
    expect(html).toContain('type="text"');
    expect(html).toContain('value="eleven_flash_v2_5"');
    expect(html).not.toContain("<select");
  });
});
