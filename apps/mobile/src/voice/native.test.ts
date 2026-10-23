import { describe, expect, it, vi } from "vitest";

vi.mock("expo-modules-core", () => ({
  requireNativeModule: () => {
    throw new Error("no native module in this environment");
  },
}));

const { base64, NativeAudioSink, Dictation, dictationAvailable, releaseAudio, voiceBuiltIn } =
  await import("./native");

describe("base64", () => {
  // Frames are raw audio, so every byte value has to survive and the
  // padding has to be right or the native decoder drops the frame.
  it("encodes bytes the way a decoder expects", () => {
    expect(base64(new Uint8Array([]))).toBe("");
    expect(base64(new Uint8Array([77]))).toBe("TQ==");
    expect(base64(new Uint8Array([77, 97]))).toBe("TWE=");
    expect(base64(new Uint8Array([77, 97, 110]))).toBe("TWFu");
  });

  it("encodes high bytes that a string-based encoder would reject", () => {
    expect(base64(new Uint8Array([0xff, 0xfe, 0xfd]))).toBe("//79");
    expect(base64(new Uint8Array([0, 0, 0]))).toBe("AAAA");
  });
});

// A build without the native module still has to work as a text client,
// so nothing here may throw.
describe("without the native module", () => {
  it("reports that it cannot listen", () => {
    expect(dictationAvailable()).toBe(false);
  });

  it("reports that voice is not built in, so the app offers no Voice screen", () => {
    expect(voiceBuiltIn()).toBe(false);
  });

  it("ends an utterance without failing, so a call can still be hung up", () => {
    expect(() => new Dictation().nextUtterance()).not.toThrow();
  });

  it("hands the microphone back without failing", () => {
    expect(() => releaseAudio()).not.toThrow();
  });

  it("swallows playback calls rather than crashing the panel", () => {
    const sink = new NativeAudioSink();
    expect(() => {
      sink.begin("pcm_s16le", 24000);
      sink.push(new Uint8Array([1, 2, 3]));
      sink.end();
      sink.stop();
    }).not.toThrow();
  });

  it("tells the user to type instead of failing silently", async () => {
    const errors: string[] = [];
    await new Dictation().start(
      () => {},
      (message) => errors.push(message),
    );
    expect(errors).toEqual(["this build cannot listen, so type instead"]);
  });
});

/** A module whose permission prompt resolves only when told to. */
function slowToPermit() {
  let grant: (allowed: boolean) => void = () => {};
  const calls: string[] = [];
  const expected: string[][] = [];
  const native = {
    dictationAvailable: () => true,
    requestPermissions: () =>
      new Promise<boolean>((resolve) => {
        grant = resolve;
      }),
    startListening: async (inCall: boolean, vocabulary: string[]) => {
      calls.push("start");
      expected.push(vocabulary);
    },
    stopListening: () => calls.push("stop"),
    endUtterance: () => calls.push("utterance"),
    beginSpeech: () => {},
    pushSpeech: () => {},
    endSpeech: () => {},
    stopSpeech: () => {},
    addListener: () => ({ remove: () => {} }),
  };
  return { native, calls, expected, grant: (allowed: boolean) => grant(allowed) };
}

describe("holding to talk", () => {
  /**
   * The first hold races the permission dialog. Releasing before it is
   * answered used to start listening anyway, leaving the microphone open
   * with nobody reading what it heard.
   */
  it("does not open the microphone when the hold ended during the prompt", async () => {
    const { native, calls, grant } = slowToPermit();
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const dictation = new Dictation(native as any);

    const holding = dictation.start(
      () => {},
      () => {},
    );
    dictation.stop();
    grant(true);
    await holding;

    expect(calls).not.toContain("start");
  });

  /** The ordinary case still listens once permission is given. */
  it("listens when the hold is still on by the time permission lands", async () => {
    const { native, calls, grant } = slowToPermit();
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const dictation = new Dictation(native as any);

    const holding = dictation.start(
      () => {},
      () => {},
    );
    grant(true);
    await holding;

    expect(calls).toContain("start");
  });

  /// The words recognition is told to expect have to reach it, or a name
  /// it has never heard arrives as the nearest ordinary word.
  it("hands the vocabulary to recognition", async () => {
    const { native, expected, grant } = slowToPermit();
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const dictation = new Dictation(native as any);

    const holding = dictation.start(
      () => {},
      () => {},
      false,
      ["iDEN", "phone 1000"],
    );
    grant(true);
    await holding;

    expect(expected).toEqual([["iDEN", "phone 1000"]]);
  });
});

describe("in a call", () => {
  /**
   * A call keeps the microphone open across utterances, so what has been
   * acted on has to be cut from the transcript. Left in, it comes back
   * as the beginning of the next utterance and is sent a second time.
   */
  it("cuts the transcript at the end of an utterance without letting go", async () => {
    const { native, calls, grant } = slowToPermit();
    // eslint-disable-next-line @typescript-eslint/no-explicit-any
    const dictation = new Dictation(native as any);

    const listening = dictation.start(
      () => {},
      () => {},
    );
    grant(true);
    await listening;
    dictation.nextUtterance();

    expect(calls).toEqual(["start", "utterance"]);
  });
});
