import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import type { VoiceController } from "@puppet-master/client-core/voice/controller";
import { dictationAvailable, listenOnce } from "../voice/speech";
import "./VoicePanel.css";

/**
 * Talking to Puppet Master.
 *
 * Typing and speaking go to the same place: the transcript is the interface,
 * and dictation is one way of filling it. That keeps the panel usable in
 * browsers without speech recognition, and testable without audio.
 *
 * The controller outlives the panel, because an agent's answer can arrive
 * minutes after it was asked for and closing the panel must not lose it.
 * Closing does stop whatever is being read out at the time.
 *
 * It hangs off the topbar as an overlay rather than taking a column in
 * the layout: it is opened for a question and dismissed, so it should
 * not push the dashboard around while it is up.
 */
export function VoicePanel({
  controller,
  onClose,
}: {
  controller: VoiceController;
  onClose: () => void;
}) {
  const [speak, setSpeak] = useState(true);
  const [draft, setDraft] = useState("");
  const [listening, setListening] = useState(false);
  const [trouble, setTrouble] = useState("");
  const [, bump] = useState(0);
  const transcriptRef = useRef<HTMLDivElement | null>(null);

  useEffect(() => {
    const stop = controller.subscribe(() => bump((n) => n + 1));
    return () => {
      stop();
      // Closing the panel stops an answer being read out. The user is
      // done listening, and speech carrying on behind a panel that is
      // no longer there is not what closing means.
      void controller.cancel();
    };
  }, [controller]);

  // An overlay is dismissed the way every other overlay is: Escape, or
  // a click past it. The toggle it hangs from is excluded, or opening
  // and closing would fight each other on the same click.
  useEffect(() => {
    const onKey = (event: KeyboardEvent) => {
      if (event.key === "Escape") onClose();
    };
    const onPointerDown = (event: PointerEvent) => {
      const target = event.target as Element | null;
      if (target?.closest(".voice-panel") || target?.closest(".voice-toggle")) return;
      onClose();
    };
    window.addEventListener("keydown", onKey);
    window.addEventListener("pointerdown", onPointerDown);
    return () => {
      window.removeEventListener("keydown", onKey);
      window.removeEventListener("pointerdown", onPointerDown);
    };
  }, [onClose]);

  const state = controller.state();

  useEffect(() => {
    // Follow the conversation as it grows, the way a chat does.
    const transcript = transcriptRef.current;
    if (transcript) transcript.scrollTop = transcript.scrollHeight;
  }, [state.lines.length]);

  const send = useCallback(
    async (text: string) => {
      setDraft("");
      await controller.say(text, speak);
    },
    [controller, speak],
  );

  const dictate = useCallback(async () => {
    setTrouble("");
    // Taking the floor: an answer still being read out stops rather
    // than playing on over the user.
    if (controller.state().status !== "idle") await controller.cancel();
    setListening(true);
    try {
      const heard = await listenOnce();
      if (heard) await send(heard);
    } catch (err) {
      setTrouble(err instanceof Error ? err.message : String(err));
    } finally {
      setListening(false);
    }
  }, [controller, send]);

  const busy = state.status !== "idle";
  const canDictate = useMemo(dictationAvailable, []);

  return (
    <aside className="voice-panel" aria-label="Voice">
      <header className="voice-panel-head">
        <h2>Voice</h2>
        <label className="voice-speak-toggle">
          <input
            type="checkbox"
            checked={speak}
            onChange={(event) => setSpeak(event.target.checked)}
          />
          speak answers
        </label>
        <button type="button" className="btn" onClick={onClose} aria-label="Close voice">
          Close
        </button>
      </header>

      <div className="voice-transcript" ref={transcriptRef} data-testid="voice-transcript">
        {state.lines.length === 0 && (
          <p className="voice-empty">
            Ask what needs you, what something is doing, or tell it to start work.
          </p>
        )}
        {state.lines.map((line, index) => (
          <p key={index} className={`voice-line voice-line-${line.from}`}>
            {line.text}
          </p>
        ))}
        {state.status === "thinking" && (
          <p className="voice-line voice-line-agent voice-pending">thinking</p>
        )}
      </div>

      {(state.error || trouble) && (
        <p className="voice-trouble" role="status">
          {state.error || trouble}
        </p>
      )}

      <form
        className="voice-compose"
        onSubmit={(event) => {
          event.preventDefault();
          void send(draft);
        }}
      >
        <input
          type="text"
          value={draft}
          placeholder="Say something, or type it"
          aria-label="What to say"
          onChange={(event) => setDraft(event.target.value)}
        />
        {canDictate && (
          <button
            type="button"
            className="btn"
            onClick={() => void dictate()}
            disabled={listening}
            aria-pressed={listening}
          >
            {listening ? "Listening" : "Talk"}
          </button>
        )}
        <button type="submit" className="btn" disabled={!draft.trim()}>
          Send
        </button>
        {busy && (
          <button type="button" className="btn" onClick={() => void controller.cancel()}>
            Stop
          </button>
        )}
      </form>
    </aside>
  );
}
