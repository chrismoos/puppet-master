import { describe, expect, it } from "vitest";
import { create } from "@bufbuild/protobuf";
import { VoiceDeltaSchema } from "../gen/pm/v1/pm_pb";
import { VoiceController, type AudioSink, type VoiceTransport } from "./controller";

interface DeltaFields {
  text?: string;
  audio?: Uint8Array;
  audioCodec?: string;
  audioSampleRate?: number;
  done?: boolean;
  error?: string;
}

function delta(fields: DeltaFields) {
  return create(VoiceDeltaSchema, { conversationId: 7n, ...fields });
}

class FakeSink implements AudioSink {
  began: Array<[string, number]> = [];
  frames: Uint8Array[] = [];
  ended = 0;
  stopped = 0;
  begin(codec: string, sampleRate: number) {
    this.began.push([codec, sampleRate]);
  }
  push(frame: Uint8Array) {
    this.frames.push(frame);
  }
  end() {
    this.ended += 1;
  }
  stop() {
    this.stopped += 1;
  }
}

function transport(overrides: Partial<VoiceTransport> = {}) {
  let emit: (d: ReturnType<typeof delta>) => void = () => {};
  const sent: string[] = [];
  const canceled: bigint[] = [];
  const base: VoiceTransport = {
    voiceTurn: async (text) => {
      sent.push(text);
      return 7n;
    },
    voiceCancel: async (id) => {
      canceled.push(id);
    },
    onVoiceDelta: (listener) => {
      emit = listener;
      return () => {};
    },
    ...overrides,
  };
  return { transport: base, sent, canceled, emit: (d: ReturnType<typeof delta>) => emit(d) };
}

describe("VoiceController", () => {
  it("shows what was said before the answer arrives", async () => {
    const t = transport();
    const controller = new VoiceController(t.transport);
    controller.start();
    await controller.say("what needs me", false);
    expect(t.sent).toEqual(["what needs me"]);
    expect(controller.state().lines).toEqual([{ from: "user", text: "what needs me" }]);
    expect(controller.state().status).toBe("thinking");
  });

  it("ignores an utterance that is only whitespace", async () => {
    const t = transport();
    const controller = new VoiceController(t.transport);
    controller.start();
    await controller.say("   ", false);
    expect(t.sent).toEqual([]);
    expect(controller.state().lines).toEqual([]);
  });

  // Text arrives in pieces as the agent produces it. Each piece as its own
  // line would make the transcript unreadable.
  it("joins consecutive pieces of one answer into a line", async () => {
    const t = transport();
    const controller = new VoiceController(t.transport);
    controller.start();
    await controller.say("status", false);
    t.emit(delta({ text: "Nothing needs you." }));
    t.emit(delta({ text: "Two sessions are working." }));
    expect(controller.state().lines).toEqual([
      { from: "user", text: "status" },
      { from: "agent", text: "Nothing needs you. Two sessions are working." },
    ]);
  });

  it("plays frames in the codec the stream named", async () => {
    const t = transport();
    const sink = new FakeSink();
    const controller = new VoiceController(t.transport, sink);
    controller.start();
    await controller.say("status", true);
    t.emit(delta({ audioCodec: "pcm_s16le", audioSampleRate: 24000 }));
    t.emit(delta({ audio: new Uint8Array([1, 2]) }));
    t.emit(delta({ audio: new Uint8Array([3, 4]) }));
    expect(controller.state().status).toBe("speaking");
    t.emit(delta({ done: true }));
    expect(sink.began).toEqual([["pcm_s16le", 24000]]);
    expect(sink.frames.map((f) => Array.from(f))).toEqual([[1, 2], [3, 4]]);
    expect(sink.ended).toBe(1);
    expect(controller.state().status).toBe("idle");
  });

  // Barging in has to silence what is already playing, or the user talks
  // over the previous answer.
  it("stops playback when the user interrupts", async () => {
    const t = transport();
    const sink = new FakeSink();
    const controller = new VoiceController(t.transport, sink);
    controller.start();
    await controller.say("status", true);
    t.emit(delta({ audioCodec: "pcm_s16le", audioSampleRate: 24000 }));
    await controller.say("actually never mind", true);
    expect(sink.stopped).toBeGreaterThan(0);
    expect(t.canceled).toEqual([7n]);
    expect(t.sent).toEqual(["status", "actually never mind"]);
  });

  it("surfaces the reason a turn stopped badly", async () => {
    const t = transport();
    const controller = new VoiceController(t.transport);
    controller.start();
    await controller.say("status", false);
    t.emit(delta({ error: "could not reach the model", done: true }));
    expect(controller.state().error).toBe("could not reach the model");
    expect(controller.state().status).toBe("idle");
  });

  it("reports a send that failed outright", async () => {
    const t = transport({
      voiceTurn: async () => {
        throw new Error("socket is closed");
      },
    });
    const controller = new VoiceController(t.transport);
    controller.start();
    await controller.say("status", false);
    expect(controller.state().error).toBe("socket is closed");
    expect(controller.state().status).toBe("idle");
  });

  // A shared connection can carry another conversation's answer.
  it("ignores deltas for a different conversation", async () => {
    const t = transport();
    const controller = new VoiceController(t.transport);
    controller.start();
    await controller.say("status", false);
    t.emit(create(VoiceDeltaSchema, { conversationId: 99n, text: "not yours" }));
    expect(controller.state().lines).toEqual([{ from: "user", text: "status" }]);
  });
});

describe("a codec the device cannot play", () => {
  class Mp3OnlySink extends FakeSink {
    supports(codec: string) {
      return codec === "pcm_s16le";
    }
  }

  // Silence with nothing said about it reads as a broken feature. The
  // words still arrived, so the honest outcome is to say that.
  it("says so instead of playing nothing", async () => {
    const t = transport();
    const sink = new Mp3OnlySink();
    const controller = new VoiceController(t.transport, sink);
    controller.start();
    await controller.say("status", true);
    t.emit(delta({ text: "All quiet." }));
    t.emit(delta({ audioCodec: "mp3", audioSampleRate: 44100 }));
    t.emit(delta({ audio: new Uint8Array([9, 9]) }));
    t.emit(delta({ done: true }));

    expect(controller.state().error).toContain("cannot play mp3");
    expect(sink.began).toEqual([]);
    expect(sink.frames).toEqual([]);
    expect(controller.state().lines).toEqual([
      { from: "user", text: "status" },
      { from: "agent", text: "All quiet." },
    ]);
  });

  it("still plays a codec it supports", async () => {
    const t = transport();
    const sink = new Mp3OnlySink();
    const controller = new VoiceController(t.transport, sink);
    controller.start();
    await controller.say("status", true);
    t.emit(delta({ audioCodec: "pcm_s16le", audioSampleRate: 24000 }));
    t.emit(delta({ audio: new Uint8Array([1, 2]) }));
    t.emit(delta({ done: true }));

    expect(controller.state().error).toBe("");
    expect(sink.frames.map((f) => Array.from(f))).toEqual([[1, 2]]);
  });
});
