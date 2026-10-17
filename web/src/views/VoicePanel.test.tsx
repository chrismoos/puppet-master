// @vitest-environment jsdom
import { act } from "react";
import { createRoot, type Root } from "react-dom/client";
import { afterEach, beforeEach, expect, it, vi } from "vitest";
import { create } from "@bufbuild/protobuf";
import { VoiceDeltaSchema, type VoiceDelta } from "@puppet-master/client-core/gen/pm/v1/pm_pb";
import type { PmClient } from "@puppet-master/client-core/ws/client";
import { VoiceController } from "@puppet-master/client-core/voice/controller";
import { VoicePanel } from "./VoicePanel";

// The panel plays rendered speech, and jsdom has no audio clock.
vi.mock("../voice/webAudioSink", () => ({
  WebAudioSink: class {
    begin() {}
    push() {}
    end() {}
    stop() {}
  },
}));
let canDictate = false;
vi.mock("../voice/speech", () => ({
  dictationAvailable: () => canDictate,
  listenOnce: vi.fn(async () => ""),
}));

let host: HTMLDivElement;
let root: Root;
let emit: (delta: VoiceDelta) => void;
let sent: string[];
let controller: VoiceController;

/** The controller App owns, started as App starts it. */
function started(client: PmClient): VoiceController {
  controller = new VoiceController(client);
  controller.start();
  return controller;
}

function fakeClient(): PmClient {
  sent = [];
  return {
    voiceTurn: vi.fn(async (text: string) => {
      sent.push(text);
      return 3n;
    }),
    voiceCancel: vi.fn(async () => ({})),
    onVoiceDelta: (listener: (delta: VoiceDelta) => void) => {
      emit = listener;
      return () => {};
    },
  } as unknown as PmClient;
}

beforeEach(() => {
  canDictate = false;
  host = document.createElement("div");
  document.body.append(host);
  root = createRoot(host);
});

afterEach(() => {
  act(() => root.unmount());
  host.remove();
});

function type(text: string) {
  const input = host.querySelector<HTMLInputElement>('input[type="text"]')!;
  const setter = Object.getOwnPropertyDescriptor(
    HTMLInputElement.prototype,
    "value",
  )!.set!;
  setter.call(input, text);
  input.dispatchEvent(new Event("input", { bubbles: true }));
}

function submit() {
  host.querySelector("form")!.dispatchEvent(new Event("submit", { bubbles: true, cancelable: true }));
}

it("sends what was typed and shows it straight away", async () => {
  const client = fakeClient();
  await act(async () => {
    root.render(<VoicePanel controller={started(client)} onClose={() => {}} />);
  });
  await act(async () => type("what needs me"));
  await act(async () => submit());

  expect(sent).toEqual(["what needs me"]);
  expect(host.textContent).toContain("what needs me");
});

it("renders the answer as it streams in", async () => {
  const client = fakeClient();
  await act(async () => {
    root.render(<VoicePanel controller={started(client)} onClose={() => {}} />);
  });
  await act(async () => type("status"));
  await act(async () => submit());
  await act(async () => {
    emit(create(VoiceDeltaSchema, { conversationId: 3n, text: "Nothing needs you." }));
    emit(create(VoiceDeltaSchema, { conversationId: 3n, done: true }));
  });

  expect(host.textContent).toContain("Nothing needs you.");
});

// A failure the user needs to act on has to be visible, not only logged.
it("shows why a turn stopped badly", async () => {
  const client = fakeClient();
  await act(async () => {
    root.render(<VoicePanel controller={started(client)} onClose={() => {}} />);
  });
  await act(async () => type("status"));
  await act(async () => submit());
  await act(async () => {
    emit(
      create(VoiceDeltaSchema, {
        conversationId: 3n,
        error: "no model profile is set for the voice agent",
        done: true,
      }),
    );
  });

  expect(host.textContent).toContain("no model profile is set for the voice agent");
});

/// Closing the panel while an answer is being read out has to silence
/// it: the speech is already in the sink, so stopping the turn alone
/// would leave it talking behind a panel that is gone.
it("stops an answer being read out when the panel is closed", async () => {
  const client = fakeClient();
  let stopped = 0;
  const sink = {
    begin: () => {},
    push: () => {},
    end: () => {},
    stop: () => {
      stopped += 1;
    },
  };
  const own = new VoiceController(client, sink);
  own.start();
  await act(async () => {
    root.render(<VoicePanel controller={own} onClose={() => {}} />);
  });
  await act(async () => type("status"));
  await act(async () => submit());
  await act(async () => {
    emit(
      create(VoiceDeltaSchema, {
        conversationId: 3n,
        audioCodec: "pcm_s16le",
        audioSampleRate: 24_000,
      }),
    );
  });
  const beforeClose = stopped;

  await act(async () => root.unmount());

  expect(stopped).toBeGreaterThan(beforeClose);
  expect(client.voiceCancel).toHaveBeenCalledWith(3n);
});

it("does not send an empty utterance", async () => {
  const client = fakeClient();
  await act(async () => {
    root.render(<VoicePanel controller={started(client)} onClose={() => {}} />);
  });
  await act(async () => submit());
  expect(sent).toEqual([]);
});

// Without dictation the text box is the whole interface, so the button that
// needs a microphone is not offered.
it("offers no talk button when the browser cannot dictate", async () => {
  const client = fakeClient();
  await act(async () => {
    root.render(<VoicePanel controller={started(client)} onClose={() => {}} />);
  });
  expect(host.textContent).not.toContain("Talk");
});

/**
 * An agent's answer can arrive minutes after it was asked for, long after
 * the user has closed the panel. The controller outlives the panel for
 * exactly this reason: when it was created per mount, closing the panel
 * unsubscribed it and the answer was dropped with nothing said.
 */
it("keeps an answer that arrives while the panel is closed", async () => {
  const client = fakeClient();
  const live = started(client);
  await act(async () => {
    root.render(<VoicePanel controller={live} onClose={() => {}} />);
  });
  await act(async () => type("ask it what it is doing"));
  await act(async () => submit());

  // The user closes the panel while the agent is still thinking.
  await act(async () => root.render(<></>));
  expect(host.textContent).not.toContain("Checking in phone 1000 now.");

  await act(async () => {
    emit(
      create(VoiceDeltaSchema, {
        conversationId: 3n,
        text: "Porting the iden stack says: Checking in phone 1000 now.",
      }),
    );
    emit(create(VoiceDeltaSchema, { conversationId: 3n, done: true }));
  });

  // Reopening shows it, because the transcript was never thrown away.
  await act(async () => {
    root.render(<VoicePanel controller={live} onClose={() => {}} />);
  });
  expect(host.textContent).toContain("Checking in phone 1000 now.");
});

/**
 * An overlay is dismissed the way every other overlay is. The toggle it
 * hangs from is excluded, or the same click would close and reopen it.
 */
it("closes on Escape and on a click past it", async () => {
  const client = fakeClient();
  let closed = 0;
  await act(async () => {
    root.render(<VoicePanel controller={started(client)} onClose={() => (closed += 1)} />);
  });

  await act(async () => {
    window.dispatchEvent(new KeyboardEvent("keydown", { key: "Escape" }));
  });
  expect(closed).toBe(1);

  const outside = document.createElement("div");
  document.body.append(outside);
  await act(async () => {
    outside.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
  });
  expect(closed).toBe(2);

  // A click inside the panel is not a dismissal.
  const inside = host.querySelector(".voice-panel")!;
  await act(async () => {
    inside.dispatchEvent(new PointerEvent("pointerdown", { bubbles: true }));
  });
  expect(closed).toBe(2);

  outside.remove();
});

/**
 * The microphone takes the floor. Tapping it while an answer is being
 * read out used to leave it playing, so the user talked over the agent
 * and the recognizer heard both.
 */
it("stops an answer being read out when the microphone is tapped", async () => {
  canDictate = true;
  const client = fakeClient();
  await act(async () => {
    root.render(<VoicePanel controller={started(client)} onClose={() => {}} />);
  });
  await act(async () => type("status"));
  await act(async () => submit());
  await act(async () => {
    emit(
      create(VoiceDeltaSchema, {
        conversationId: 3n,
        audioCodec: "pcm_s16le",
        audioSampleRate: 24000,
      }),
    );
  });

  const talk = [...host.querySelectorAll("button")].find(
    (button) => button.textContent === "Talk",
  )!;
  await act(async () => {
    talk.click();
  });

  expect(client.voiceCancel).toHaveBeenCalledWith(3n);
});
