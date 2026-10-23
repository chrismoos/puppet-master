import { createRequire } from "node:module";
import { describe, expect, it } from "vitest";

const require = createRequire(import.meta.url);
const { voicePodfile, voiceInfoPlist } = require("./index.js") as {
  voicePodfile(contents: string, enabled: boolean): string;
  voiceInfoPlist(plist: Record<string, unknown>, enabled: boolean): Record<string, unknown>;
};

const PODFILE = "target 'PuppetMaster' do\n  use_expo_modules!\n  config = use_native_modules!\nend\n";

describe("voice Podfile", () => {
  it("leaves the voice module out of a build without voice", () => {
    expect(voicePodfile(PODFILE, false)).toContain("  use_expo_modules!({ exclude: ['voice'] })\n");
  });

  it("links it again when a project made without voice is rebuilt with it", () => {
    expect(voicePodfile(voicePodfile(PODFILE, false), true)).toBe(PODFILE);
  });

  it("changes nothing when run twice over the same project", () => {
    const once = voicePodfile(PODFILE, false);
    expect(voicePodfile(once, false)).toBe(once);
    expect(voicePodfile(PODFILE, true)).toBe(PODFILE);
  });

  it("fails rather than linking voice when the Podfile has no autolinking line", () => {
    expect(() => voicePodfile("target 'PuppetMaster' do\nend\n", false)).toThrow(/use_expo_modules!/);
  });
});

describe("voice Info.plist", () => {
  it("declares no microphone or speech use without voice", () => {
    const plist = voiceInfoPlist({
      NSMicrophoneUsageDescription: "left from a voice build",
      NSSpeechRecognitionUsageDescription: "left from a voice build",
      ITSAppUsesNonExemptEncryption: false,
    }, false);
    expect(plist).toEqual({ ITSAppUsesNonExemptEncryption: false });
  });

  it("declares both with voice, keeping a string the app config already set", () => {
    const plist = voiceInfoPlist({ NSMicrophoneUsageDescription: "custom" }, true);
    expect(plist.NSMicrophoneUsageDescription).toBe("custom");
    expect(plist.NSSpeechRecognitionUsageDescription).toEqual(expect.any(String));
  });
});
