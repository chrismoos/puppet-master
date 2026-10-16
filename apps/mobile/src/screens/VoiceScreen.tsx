import { useCallback, useEffect, useMemo, useRef, useState } from "react";
import {
  Pressable,
  ScrollView,
  StyleSheet,
  Text,
  TextInput,
  View,
} from "react-native";

import type { VoiceController } from "@puppet-master/client-core/voice/controller";

import {
  Dictation,
  audioRoute,
  dictationAvailable,
  prepareSpeech,
  setOutputToSpeaker,
} from "../voice/native";
import { CallTurns } from "../voice/call";
import { SilenceTimer } from "../voice/silence";
import { colors } from "../theme";
import { VoiceControlButton } from "./VoiceControlButton";
import { composeControls, headControls, type VoiceControl, type VoiceUi } from "./voiceControls";

/// How long to wait for the user to start speaking before giving up, so
/// a tap by mistake does not hold the microphone open.
const WAITING_FOR_SPEECH_MS = 10_000;

/// How long a pause ends a sentence, once something has been heard. Long
/// enough for the recognizer's last correction to land, short enough
/// that the user is not waiting on their own words being sent.
const SENTENCE_ENDED_MS = 1_500;

/**
 * Talking to Puppet Master from the phone.
 *
 * Recognition runs on this device and only its text goes up, so no audio
 * is uploaded. Speech comes back already rendered by the daemon, which
 * keeps the provider credential in one place and makes every client
 * sound the same.
 *
 * Two ways to talk. Tap to talk sends when the user stops speaking: a
 * pause of a second and a half after something has been heard ends the
 * sentence, and ten seconds with nothing heard gives up. Tapping again
 * sends immediately.
 *
 * A call keeps the microphone open instead. Utterances send themselves,
 * the answer can be talked over, and anything said while the agent is
 * working is held and sent as one turn when it finishes. The native
 * side keeps echo cancellation on for a call, because the speaker is
 * live while the microphone is listening.
 *
 * Either way the microphone is never open without the user opening it,
 * and what it hears fills the compose box as it goes.
 *
 * The controller outlives this screen, because an agent's answer can
 * arrive minutes after it was asked for and leaving the screen must not
 * lose it.
 */
export function VoiceScreen({
  controller,
  onClose,
}: {
  controller: VoiceController;
  onClose: () => void;
}) {
  const [draft, setDraft] = useState("");
  const [listening, setListening] = useState(false);
  const [inCall, setInCall] = useState(false);
  const [speaker, setSpeaker] = useState(true);
  const [trouble, setTrouble] = useState("");
  const [, bump] = useState(0);
  const dictation = useMemo(() => new Dictation(), []);
  // Declared before the timer that calls it, and reassigned below once
  // `finish` exists: the timer outlives any one render, so it must not
  // hold that render's closure.
  const finishRef = useRef<() => Promise<void>>(async () => {});
  const silence = useMemo(() => new SilenceTimer(() => void finishRef.current()), []);
  const turns = useMemo(() => new CallTurns(), []);
  const canTalk = useMemo(dictationAvailable, []);
  const transcript = useRef<ScrollView | null>(null);
  // What dictation last heard, read when listening ends rather than
  // through state, which has not settled by then.
  const latest = useRef("");
  // Whether a call is running, read from callbacks that outlive the
  // render they were made in.
  const callRef = useRef(false);

  useEffect(() => {
    const stop = controller.subscribe(() => bump((n) => n + 1));
    return () => {
      stop();
      dictation.stop();
      silence.cancel();
    };
  }, [controller, dictation, silence]);

  useEffect(() => {
    // Asked for on arrival rather than on the first tap, so the first
    // thing the user says is not lost behind a permission dialog.
    if (canTalk) void dictation.permitted();
  }, [canTalk, dictation]);

  useEffect(() => {
    // The audio path is readied while the screen opens, so the first
    // answer is not waiting on the engine to start.
    prepareSpeech();
  }, []);

  const state = controller.state();

  // What the agent is reading out. A call hears its own speaker, so the
  // turn rules need to know what the words coming back were.
  const spoken = useMemo(() => {
    const last = state.lines[state.lines.length - 1];
    return last?.from === "agent" ? last.text : "";
  }, [state.lines]);

  useEffect(() => {
    if (inCall) turns.spoke(spoken);
  }, [inCall, spoken, turns]);

  useEffect(() => {
    if (!inCall) return;
    if (state.status === "thinking") {
      turns.working();
      return;
    }
    if (state.status === "speaking") {
      turns.speaking();
      return;
    }
    // Idle: anything said while the agent worked goes out now, as one
    // turn, rather than as answers to questions already forgotten.
    const held = turns.idle();
    if (held) void sendRef.current(held);
  }, [inCall, state.status, turns]);

  const send = useCallback(
    async (text: string) => {
      setDraft("");
      latest.current = "";
      await controller.say(text, true);
    },
    [controller],
  );
  // Read by the phase effect, which must not re-run every time `send`
  // is rebuilt.
  const sendRef = useRef(send);
  sendRef.current = send;

  /// Ends an utterance and acts on it. Nothing heard sends nothing, so
  /// a tap by mistake, or a call nobody spoke into, costs nothing.
  ///
  /// In a call the microphone stays open: what the utterance does
  /// depends on whether the agent is working or speaking, which is what
  /// `CallTurns` decides.
  const finish = useCallback(async () => {
    silence.cancel();
    const said = latest.current.trim();
    latest.current = "";
    if (!callRef.current) {
      setListening(false);
      dictation.stop();
      if (said) await send(said);
      return;
    }
    setDraft("");
    // The microphone stays open, so the transcript has to be cut here
    // or what was just said comes back as the start of the next
    // utterance and is acted on twice.
    if (said) dictation.nextUtterance();
    const action = turns.heard(said);
    // Listening again at once, so the next thing said is not missed
    // while the agent works on this one.
    silence.restart(WAITING_FOR_SPEECH_MS);
    if (action.kind === "interrupt") {
      // Stops the speech in flight, including synthesis still being
      // paid for, before the new turn starts.
      await controller.cancel();
    }
    if (action.kind === "send" || action.kind === "interrupt") {
      await send(action.text);
    }
  }, [controller, dictation, send, silence, turns]);
  finishRef.current = finish;

  const listen = useCallback(
    async (forCall = false) => {
      setTrouble("");
      setDraft("");
      latest.current = "";
      setListening(true);
      silence.restart(WAITING_FOR_SPEECH_MS);
      await dictation.start(
        (text) => {
          latest.current = text;
          // Into the box the user is already looking at, so they can see
          // it is hearing them before it is sent.
          setDraft(text);
          // Something has been heard, so a short pause now means the
          // sentence is over rather than that it never started.
          silence.restart(text.trim() ? SENTENCE_ENDED_MS : WAITING_FOR_SPEECH_MS);
        },
        (message) => {
          silence.cancel();
          setTrouble(message);
          setListening(false);
          setInCall(false);
          callRef.current = false;
        },
        forCall,
      );
    },
    [dictation, silence],
  );

  /// Tapping the microphone takes the floor, so an answer being read out
  /// stops rather than playing on over what the user is saying.
  const talk = useCallback(async () => {
    if (state.status !== "idle") await controller.cancel();
    await listen();
  }, [controller, listen, state.status]);

  /// Starts a call: the microphone stays open, utterances send
  /// themselves, and the answer can be talked over.
  const startCall = useCallback(async () => {
    // Starting a call switches the echo canceller on, which rebuilds the
    // audio graph, so an answer still being read out stops here rather
    // than part way through a sentence.
    if (state.status !== "idle") await controller.cancel();
    turns.reset();
    setInCall(true);
    callRef.current = true;
    // Headphones or a car are where the user wants it; only a bare
    // handset starts on the speaker.
    const connected = audioRoute();
    const ownSpeaker = connected !== "headphones" && connected !== "car";
    setSpeaker(ownSpeaker);
    setOutputToSpeaker(ownSpeaker);
    await listen(true);
  }, [controller, listen, state.status, turns]);

  const endCall = useCallback(async () => {
    callRef.current = false;
    setInCall(false);
    setListening(false);
    silence.cancel();
    dictation.stop();
    turns.reset();
    await controller.cancel();
  }, [controller, dictation, silence, turns]);

  const ui: VoiceUi = { canTalk, inCall, listening, speaker, busy: state.status !== "idle" };

  const press = (control: VoiceControl) => {
    switch (control) {
      case "call":
        void (inCall ? endCall() : startCall());
        return;
      case "route": {
        const next = !speaker;
        setSpeaker(next);
        setOutputToSpeaker(next);
        return;
      }
      case "talk":
        void (listening ? finish() : talk());
        return;
      case "stop":
        void controller.cancel();
    }
  };

  return (
    <View style={styles.screen}>
      <View style={styles.head}>
        <Text style={styles.title}>Voice</Text>
        <View style={styles.headControls}>
          {headControls(ui).map((control) => (
            <VoiceControlButton
              key={control}
              control={control}
              ui={ui}
              onPress={() => press(control)}
            />
          ))}
          <Pressable onPress={onClose} accessibilityLabel="Close voice" hitSlop={8}>
            <Text style={styles.close}>Close</Text>
          </Pressable>
        </View>
      </View>

      <ScrollView
        ref={transcript}
        style={styles.transcript}
        onContentSizeChange={() => transcript.current?.scrollToEnd({ animated: true })}
      >
        {state.lines.length === 0 && (
          <Text style={styles.empty}>
            Ask what needs you, what something is doing, or tell it to start work.
          </Text>
        )}
        {state.lines.map((line, index) => (
          <View
            key={index}
            style={[styles.line, line.from === "user" ? styles.fromUser : styles.fromAgent]}
          >
            <Text style={styles.lineText}>{line.text}</Text>
          </View>
        ))}
        {state.status === "thinking" && <Text style={styles.pending}>thinking</Text>}
      </ScrollView>

      {(state.error || trouble) !== "" && (
        <Text style={styles.trouble}>{state.error || trouble}</Text>
      )}

      <View style={styles.compose}>
        <TextInput
          style={styles.input}
          value={draft}
          placeholder="Type, or tap to talk"
          placeholderTextColor={colors.textMuted}
          accessibilityLabel="What to say"
          onChangeText={setDraft}
          onSubmitEditing={() => {
            if (draft.trim()) void send(draft);
          }}
          returnKeyType="send"
        />
        {composeControls(ui).map((control) => (
          <VoiceControlButton
            key={control}
            control={control}
            ui={ui}
            onPress={() => press(control)}
          />
        ))}
      </View>
    </View>
  );
}

const styles = StyleSheet.create({
  screen: { flex: 1, backgroundColor: colors.bg, padding: 12, gap: 8 },
  head: { flexDirection: "row", alignItems: "center", justifyContent: "space-between" },
  headControls: { flexDirection: "row", alignItems: "center", gap: 8 },
  title: { color: colors.text, fontSize: 18, fontWeight: "600" },
  close: { color: colors.blue, fontSize: 15 },
  transcript: { flex: 1 },
  empty: { color: colors.textMuted, fontSize: 14 },
  line: { marginBottom: 8, padding: 8, borderRadius: 10, maxWidth: "88%" },
  fromUser: { alignSelf: "flex-end", backgroundColor: colors.surface },
  fromAgent: { alignSelf: "flex-start", backgroundColor: colors.panelAlt },
  lineText: { color: colors.text, fontSize: 15 },
  pending: { color: colors.textMuted, fontStyle: "italic" },
  trouble: { color: colors.red, fontSize: 14 },
  compose: { flexDirection: "row", alignItems: "center", gap: 8 },
  input: {
    flex: 1,
    color: colors.text,
    backgroundColor: colors.panelAlt,
    borderRadius: 10,
    paddingHorizontal: 10,
    paddingVertical: 8,
    fontSize: 15,
  },
});
