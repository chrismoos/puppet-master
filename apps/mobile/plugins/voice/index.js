// Expo config plugin: the usage descriptions dictation needs, or, in a
// build without voice, no microphone or speech code at all.
//
// Both strings are required for the app to ask at all. Without them the
// first request terminates the app instead of showing a prompt. App Store
// review asks where a declared microphone is used, so voice ships only
// when PM_VOICE=1 is set at prebuild. Otherwise the native module is left
// out of autolinking and its keys out of Info.plist.

const { withInfoPlist, withPodfile } = require("expo/config-plugins");

const MICROPHONE =
  "Puppet Master listens when you hold the talk button, so you can ask about your sessions.";
const SPEECH =
  "Speech is transcribed on this device so you can talk to Puppet Master. Only the text is sent.";

const PACKAGE = require("./package.json").name;
const PLAIN = "use_expo_modules!\n";
const WITHOUT_VOICE = `use_expo_modules!({ exclude: ['${PACKAGE}'] })\n`;

function voiceEnabled() {
  return process.env.PM_VOICE === "1";
}

/** The Podfile with the voice module linked or left out. Prebuild runs
 * over an existing project, so either state can be the starting one. */
function voicePodfile(contents, enabled) {
  const [from, to] = enabled ? [WITHOUT_VOICE, PLAIN] : [PLAIN, WITHOUT_VOICE];
  if (contents.includes(to)) return contents;
  if (!contents.includes(from)) {
    throw new Error(`voice plugin: the Podfile has no "${from.trim()}" line to rewrite`);
  }
  return contents.replace(from, to);
}

/** The Info.plist with the voice permission strings, or without them. */
function voiceInfoPlist(plist, enabled) {
  if (enabled) {
    plist.NSMicrophoneUsageDescription ??= MICROPHONE;
    plist.NSSpeechRecognitionUsageDescription ??= SPEECH;
  } else {
    delete plist.NSMicrophoneUsageDescription;
    delete plist.NSSpeechRecognitionUsageDescription;
  }
  return plist;
}

function withVoice(config) {
  const enabled = voiceEnabled();
  config = withPodfile(config, (mod) => {
    mod.modResults.contents = voicePodfile(mod.modResults.contents, enabled);
    return mod;
  });
  return withInfoPlist(config, (mod) => {
    mod.modResults = voiceInfoPlist(mod.modResults, enabled);
    return mod;
  });
}

module.exports = withVoice;
module.exports.voicePodfile = voicePodfile;
module.exports.voiceInfoPlist = voiceInfoPlist;
