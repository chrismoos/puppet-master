/**
 * Dictation in a browser, where it exists.
 *
 * Chrome's implementation sends audio to Google, which is a reason to prefer
 * the phone's on-device recognition and to keep typing available everywhere.
 * Absent support is not a failure: the text box is the same interface.
 */

type RecognitionLike = {
  lang: string;
  continuous: boolean;
  interimResults: boolean;
  start(): void;
  stop(): void;
  onresult: ((event: { results: ArrayLike<ArrayLike<{ transcript: string }>> }) => void) | null;
  onerror: ((event: { error?: string }) => void) | null;
  onend: (() => void) | null;
};

type RecognitionCtor = new () => RecognitionLike;

function constructor(): RecognitionCtor | undefined {
  const w = window as unknown as {
    SpeechRecognition?: RecognitionCtor;
    webkitSpeechRecognition?: RecognitionCtor;
  };
  return w.SpeechRecognition ?? w.webkitSpeechRecognition;
}

export function dictationAvailable(): boolean {
  return constructor() !== undefined;
}

/**
 * Listens once and resolves with what was heard, or an empty string when
 * nothing was. Rejects only when recognition could not run at all.
 */
export function listenOnce(): Promise<string> {
  const Ctor = constructor();
  if (!Ctor) return Promise.reject(new Error("this browser has no dictation"));
  const recognition = new Ctor();
  recognition.lang = navigator.language;
  recognition.continuous = false;
  recognition.interimResults = false;
  return new Promise((resolve, reject) => {
    let heard = "";
    let failed: string | undefined;
    recognition.onresult = (event) => {
      const results = Array.from({ length: event.results.length }, (_, i) => event.results[i]);
      heard = results
        .map((result) => result[0]?.transcript ?? "")
        .join(" ")
        .trim();
    };
    recognition.onerror = (event) => {
      failed = event.error ?? "dictation failed";
    };
    recognition.onend = () => {
      // "no-speech" is the user saying nothing, not a broken microphone.
      if (failed && failed !== "no-speech" && !heard) {
        reject(new Error(failed));
        return;
      }
      resolve(heard);
    };
    try {
      recognition.start();
    } catch (err) {
      reject(err instanceof Error ? err : new Error(String(err)));
    }
  });
}

export function stopDictation(recognition: { stop(): void } | undefined): void {
  recognition?.stop();
}
