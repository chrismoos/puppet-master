/**
 * The controls on the voice screen: where each one sits and what it
 * reads as.
 *
 * The call, and where its answers come out, sit beside the title,
 * because a call is the state the screen is in rather than something
 * done to a message. Talking and stopping sit beside the compose box,
 * with the keyboard and the thumbs.
 */
export type VoiceControl = "call" | "route" | "talk" | "stop";

export type VoiceUi = {
  canTalk: boolean;
  inCall: boolean;
  listening: boolean;
  /// Where a call sends the answer, which is only askable during one.
  speaker: boolean;
  /// Whether the agent is working on a turn or reading one out.
  busy: boolean;
};

export function headControls(ui: VoiceUi): VoiceControl[] {
  if (!ui.canTalk) return [];
  return ui.inCall ? ["route", "call"] : ["call"];
}

export function composeControls(ui: VoiceUi): VoiceControl[] {
  const controls: VoiceControl[] = [];
  // A call listens on its own, so tapping to talk during one would
  // only end an utterance the call ends by itself.
  if (ui.canTalk && !ui.inCall) controls.push("talk");
  // Talking over the answer is how a call stops it.
  if (ui.busy && !ui.inCall) controls.push("stop");
  return controls;
}

/// What the control does next rather than the state it is in: a button
/// reading "speaker" while on the speaker names nothing to act on.
export function controlLabel(control: VoiceControl, ui: VoiceUi): string {
  switch (control) {
    case "call":
      return ui.inCall ? "End call" : "Start call";
    case "route":
      return ui.speaker ? "Use earpiece" : "Use speaker";
    case "talk":
      return ui.listening ? "Stop listening" : "Talk";
    case "stop":
      return "Stop";
  }
}

/// Whether the control is lit, which is also the only hint that a call
/// is running or the microphone is open now that neither says so in
/// words.
export function controlActive(control: VoiceControl, ui: VoiceUi): boolean {
  if (control === "call") return ui.inCall;
  if (control === "talk") return ui.listening;
  return false;
}
