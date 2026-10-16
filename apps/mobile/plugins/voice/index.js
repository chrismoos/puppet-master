// Expo config plugin: the usage descriptions dictation needs.
//
// Both strings are required for the app to ask at all. Without them the
// first request terminates the app instead of showing a prompt.

const { withInfoPlist } = require("expo/config-plugins");

const MICROPHONE =
  "Puppet Master listens when you hold the talk button, so you can ask about your sessions.";
const SPEECH =
  "Speech is transcribed on this device so you can talk to Puppet Master. Only the text is sent.";

module.exports = function withVoice(config) {
  return withInfoPlist(config, (mod) => {
    mod.modResults.NSMicrophoneUsageDescription ??= MICROPHONE;
    mod.modResults.NSSpeechRecognitionUsageDescription ??= SPEECH;
    return mod;
  });
};
