import { describe, expect, it } from "vitest";

import {
  composeControls,
  controlActive,
  controlLabel,
  headControls,
  type VoiceUi,
} from "./voiceControls";

function ui(over: Partial<VoiceUi> = {}): VoiceUi {
  return {
    canTalk: true,
    inCall: false,
    listening: false,
    speaker: true,
    busy: false,
    ...over,
  };
}

describe("where the controls sit", () => {
  it("keeps the call at the top and talking at the bottom", () => {
    expect(headControls(ui())).toEqual(["route", "call"]);
    expect(composeControls(ui())).toEqual(["talk"]);
  });

  it("offers the route whether or not a call is running", () => {
    expect(headControls(ui({ inCall: true }))).toEqual(["route", "call"]);
    expect(headControls(ui())).toContain("route");
  });

  it("drops tapping to talk during a call, which listens on its own", () => {
    expect(composeControls(ui({ inCall: true }))).toEqual([]);
  });

  it("offers nothing to talk with in a build that cannot listen", () => {
    // A typed question is still answered out loud, so where it comes
    // out is the one control such a build keeps.
    expect(headControls(ui({ canTalk: false }))).toEqual(["route"]);
    expect(composeControls(ui({ canTalk: false }))).toEqual([]);
  });

  it("can stop a typed question too, but not an answer in a call", () => {
    expect(composeControls(ui({ canTalk: false, busy: true }))).toEqual(["stop"]);
    expect(composeControls(ui({ busy: true }))).toEqual(["talk", "stop"]);
    // In a call the answer is stopped by talking over it.
    expect(composeControls(ui({ inCall: true, busy: true }))).toEqual([]);
  });
});

describe("what an icon has to say for itself", () => {
  // With no words on the buttons, the label a screen reader reads is
  // the only name these controls have.
  it("names the next action rather than the current state", () => {
    expect(controlLabel("call", ui())).toBe("Start call");
    expect(controlLabel("call", ui({ inCall: true }))).toBe("End call");
    expect(controlLabel("route", ui({ speaker: true }))).toBe("Use earpiece");
    expect(controlLabel("route", ui({ speaker: false }))).toBe("Use speaker");
    expect(controlLabel("talk", ui())).toBe("Talk");
    expect(controlLabel("talk", ui({ listening: true }))).toBe("Stop listening");
    expect(controlLabel("stop", ui({ busy: true }))).toBe("Stop");
  });

  it("lights the call and the microphone while they are running", () => {
    expect(controlActive("call", ui({ inCall: true }))).toBe(true);
    expect(controlActive("talk", ui({ listening: true }))).toBe(true);
    expect(controlActive("call", ui())).toBe(false);
    expect(controlActive("talk", ui())).toBe(false);
    expect(controlActive("route", ui({ inCall: true, speaker: true }))).toBe(false);
  });
});
