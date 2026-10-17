// Dictation and speech playback for talking to Puppet Master.
//
// Recognition runs on device and only its text leaves the phone, so no
// audio is uploaded. It uses SFSpeechRecognizer rather than the more
// accurate SpeechAnalyzer, which needs a deployment target this app
// does not set yet. Playback takes the frames the controller renders
// and schedules them back to back on one clock: playing each as it
// arrives leaves audible gaps whenever the network hesitates.

import AVFoundation
import ExpoModulesCore
import Speech

/// Signed 16-bit little-endian mono, which is what the controller sends
/// unless a provider is configured to encode something else.
private let pcm16Codec = "pcm_s16le"

/// The audio session, held in one category so that listening and
/// speaking cannot flip it between them: a category change tears down a
/// running engine's graph.
///
/// The mode still differs by purpose. Measurement turns the
/// voice-processing chain off, which is what recognition wants, and
/// voice chat is the mode this app is known to produce audible output
/// under. Setting it is guarded by reading what the session already
/// carries, so it costs nothing when nothing is changing.
private enum VoiceSession {
    static func configure(mode: AVAudioSession.Mode, toSpeaker: Bool = false) throws {
        let session = AVAudioSession.sharedInstance()
        if session.category != .playAndRecord || session.mode != mode {
            try session.setCategory(
                .playAndRecord,
                mode: mode,
                options: [.defaultToSpeaker]
            )
        }
        // Always asked for, never remembered. A flag saying the session
        // is active goes stale the moment the system takes it away for
        // a call or another app, and an engine running against an
        // inactive session is silent.
        try session.setActive(true, options: .notifyOthersOnDeactivation)
        // Voice chat mode routes to the earpiece, the way a call does,
        // and the category option alone does not override it. On a phone
        // held in the hand that is indistinguishable from no sound at
        // all, and a simulator cannot show it because everything there
        // comes out of the Mac.
        //
        // Only when the earpiece is where it would otherwise go, though:
        // with headphones, a car or a speaker connected, that is where
        // the user wants their answer, and forcing the handset's own
        // speaker over them would be worse than the fault being fixed.
        if toSpeaker, session.currentRoute.outputs.allSatisfy({ output in
            output.portType == .builtInReceiver || output.portType == .builtInSpeaker
        }) {
            try? session.overrideOutputAudioPort(.speaker)
        }
    }

    /// Sends output to the handset's own speaker, or lets the system
    /// choose again. Only meaningful under `playAndRecord`, and reset by
    /// the system on a route change, so a call re-asserts it per answer.
    static func toSpeaker(_ on: Bool) {
        try? AVAudioSession.sharedInstance().overrideOutputAudioPort(on ? .speaker : .none)
    }

    /// Where output is going now, in the words a person would use.
    static func route() -> String {
        let outputs = AVAudioSession.sharedInstance().currentRoute.outputs
        guard let port = outputs.first?.portType else { return "none" }
        switch port {
        case .builtInSpeaker: return "speaker"
        case .builtInReceiver: return "earpiece"
        case .headphones, .bluetoothA2DP, .bluetoothHFP, .bluetoothLE: return "headphones"
        case .carAudio: return "car"
        default: return port.rawValue
        }
    }

    /// Gives the session up, which is what drops the recording
    /// indicator. Done when the voice screen goes away, never between
    /// answers.
    static func release() {
        try? AVAudioSession.sharedInstance().setActive(
            false,
            options: .notifyOthersOnDeactivation
        )
    }
}

/// Int16.min is one step further from zero than Int16.max, so dividing by
/// the maximum puts the quietest-possible sample past full scale.
private let int16FullScale: Float = 32768

public class PuppetVoiceModule: Module {
    private let recognizer = DictationRecognizer()
    private let player = SpeechPlayer()

    public func definition() -> ModuleDefinition {
        Name("PuppetVoice")

        Events("onTranscript", "onDictationError")

        /// Whether this device can transcribe at all. A phone without
        /// the speech model downloaded still types.
        Function("dictationAvailable") { () -> Bool in
            SFSpeechRecognizer(locale: Locale.current)?.isAvailable ?? false
        }

        AsyncFunction("requestPermissions") { () -> Bool in
            await DictationRecognizer.requestPermissions()
        }

        /// Listens until `stopListening`, reporting the transcript as it
        /// firms up. The final one arrives with `isFinal` set.
        ///
        /// The vocabulary is phrases to weigh more heavily, which is how
        /// a name recognition has never heard is not heard as the
        /// nearest ordinary word.
        AsyncFunction("startListening") { (inCall: Bool, vocabulary: [String]) in
            try self.recognizer.start(inCall: inCall, vocabulary: vocabulary) {
                [weak self] text, isFinal in
                self?.sendEvent("onTranscript", ["text": text, "isFinal": isFinal])
            } onError: { [weak self] message in
                self?.sendEvent("onDictationError", ["message": message])
            }
        }

        Function("stopListening") { () in
            self.recognizer.stop()
        }

        /// Cuts the transcript after an utterance has been acted on, so
        /// the words already sent are not reported again as part of the
        /// next one. Only a call needs this: tap to talk stops listening.
        Function("endUtterance") { () in
            self.recognizer.nextUtterance()
        }

        Function("beginSpeech") { (codec: String, sampleRate: Double) in
            self.player.begin(codec: codec, sampleRate: sampleRate)
        }

        /// One frame, base64 because the bridge carries strings far more
        /// cheaply than it carries byte arrays.
        Function("pushSpeech") { (frame: String) in
            guard let data = Data(base64Encoded: frame) else { return }
            self.player.push(data)
        }

        Function("endSpeech") { () in
            self.player.end()
        }

        Function("stopSpeech") { () in
            self.player.stop()
        }

        /// Readies the audio path before there is anything to say, so
        /// the first answer is not waiting on the session and the engine
        /// to come up.
        Function("prepareSpeech") { () in
            self.player.warm()
        }

        /// Hands the microphone and the audio session back. An active
        /// recording session is what lights the system's microphone
        /// indicator, so it is given up as soon as nothing needs it
        /// rather than held for the life of the app.
        Function("releaseAudio") { () in
            self.recognizer.stop()
            self.player.release()
        }

        /// Sends output to the handset's speaker, or hands the choice
        /// back to the system, which is what picks headphones or a car.
        Function("setOutputToSpeaker") { (on: Bool) in
            self.player.prefer(speaker: on)
        }

        /// Where output is going now, so a call can say which it is.
        Function("audioRoute") { () -> String in
            VoiceSession.route()
        }

        OnDestroy {
            self.recognizer.stop()
            self.player.release()
        }
    }
}

/// On-device dictation. Nothing here reaches the network: the request
/// requires on-device recognition, so a phone that cannot do it reports
/// unavailable rather than quietly sending audio away.
///
/// Its own engine, separate from playback's. Apple's echo canceller
/// would need both on one engine, because it cancels only what its own
/// engine plays, and routing playback through the voice-processing unit
/// makes a call's answer tick and pump. A call's own speaker is taken
/// off the microphone in the turn rules instead, by weighing what is
/// heard against what is being read out.
private final class DictationRecognizer {
    private let engine = AVAudioEngine()
    private var request: SFSpeechAudioBufferRecognitionRequest?
    private var task: SFSpeechRecognitionTask?
    private let recognizer = SFSpeechRecognizer(locale: Locale.current)
    /// Whether the button is still held. On-device recognition finalizes
    /// its task after a pause, so a hold that is still down needs
    /// another task rather than going deaf part way through a sentence.
    private var listening = false
    /// What tasks that have already finalized heard. The current task's
    /// partial results are appended to this, so the text only grows.
    private var committed = ""
    private var restarts = 0
    /// Which utterance a task belongs to. A task cancelled part way
    /// through still reports once more, and that report carries words
    /// that have already been sent. Recognition reports on a queue of
    /// its own, so this is read and written under the lock.
    private var utterance = 0
    private let turn = NSLock()
    private var onTranscript: ((String, Bool) -> Void)?
    private var onError: ((String) -> Void)?
    /// Phrases recognition weighs more heavily, carried here because
    /// every task of an utterance is given them, not just the first.
    private var vocabulary: [String] = []

    /// A recognizer that fails immediately would otherwise restart in a
    /// tight loop for as long as the button is held.
    private static let restartLimit = 20

    static func requestPermissions() async -> Bool {
        let speech = await withCheckedContinuation { continuation in
            SFSpeechRecognizer.requestAuthorization { status in
                continuation.resume(returning: status == .authorized)
            }
        }
        guard speech else { return false }
        return await withCheckedContinuation { continuation in
            AVAudioApplication.requestRecordPermission { granted in
                continuation.resume(returning: granted)
            }
        }
    }

    func start(
        inCall: Bool,
        vocabulary: [String],
        onTranscript: @escaping (String, Bool) -> Void,
        onError: @escaping (String) -> Void
    ) throws {
        stop()
        guard let recognizer, recognizer.isAvailable else {
            throw Exception(
                name: "DictationUnavailable",
                description: "this device cannot transcribe speech"
            )
        }
        self.onTranscript = onTranscript
        self.onError = onError
        self.vocabulary = vocabulary
        committed = ""
        restarts = 0
        listening = true
        let mine = beginUtterance()

        // Voice chat for a call, which plays while it records, and
        // measurement for tap to talk, whose processing-off input
        // recognizes better.
        try VoiceSession.configure(mode: inCall ? .voiceChat : .measurement)

        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        // Appends to whichever request is current rather than to the one
        // installed with: a finalized request is replaced while the tap
        // stays where it is, and audio into a dead one is heard by
        // nobody.
        input.installTap(onBus: 0, bufferSize: 1024, format: format) { [weak self] buffer, _ in
            self?.request?.append(buffer)
        }
        engine.prepare()
        try engine.start()
        listen(with: recognizer, as: mine)
    }

    private func beginUtterance() -> Int {
        turn.lock()
        defer { turn.unlock() }
        utterance += 1
        return utterance
    }

    private func isCurrent(_ which: Int) -> Bool {
        turn.lock()
        defer { turn.unlock() }
        return utterance == which
    }

    /// One recognition task, replaced as it finalizes for as long as the
    /// button is held.
    private func listen(with recognizer: SFSpeechRecognizer, as mine: Int) {
        let request = SFSpeechAudioBufferRecognitionRequest()
        request.shouldReportPartialResults = true
        request.requiresOnDeviceRecognition = true
        // Dictation rather than the default: the hint picks the language
        // model for someone talking to a device in sentences instead of
        // saying a search term.
        request.taskHint = .dictation
        request.addsPunctuation = true
        request.contextualStrings = vocabulary
        self.request = request
        task = recognizer.recognitionTask(with: request) { [weak self] result, error in
            guard let self, self.listening, self.isCurrent(mine) else { return }
            if let result {
                let heard = self.committed + result.bestTranscription.formattedString
                self.onTranscript?(heard, false)
                if result.isFinal {
                    self.committed = heard.isEmpty ? "" : heard + " "
                    self.again(with: recognizer, as: mine)
                }
            }
            if error != nil {
                // A task ending on its own mid-hold is how a pause
                // reads, not a failure the user needs to hear about.
                self.again(with: recognizer, as: mine)
            }
        }
    }

    /// Starts another task, unless something is failing fast enough that
    /// restarting is the problem rather than the fix.
    private func again(with recognizer: SFSpeechRecognizer, as mine: Int) {
        task = nil
        // The request is left in place until the next one replaces it,
        // so a buffer arriving on the audio thread in between is not
        // dropped on the floor.
        guard listening else { return }
        restarts += 1
        guard restarts <= Self.restartLimit else {
            let report = onError
            listening = false
            report?("dictation keeps stopping on this device")
            return
        }
        listen(with: recognizer, as: mine)
    }

    /// Starts the next utterance from nothing, keeping the microphone
    /// open. The replacement is installed before the old request ends so
    /// the tap always has somewhere to put what it hears.
    func nextUtterance() {
        guard listening, let recognizer else { return }
        committed = ""
        restarts = 0
        let ending = task
        let ended = request
        task = nil
        listen(with: recognizer, as: beginUtterance())
        ended?.endAudio()
        ending?.cancel()
    }

    func stop() {
        listening = false
        // Cleared before cancelling, because cancelling a task reports
        // an error the user did not cause and does not need.
        onTranscript = nil
        onError = nil
        task?.cancel()
        task = nil
        request?.endAudio()
        request = nil
        if engine.isRunning {
            engine.inputNode.removeTap(onBus: 0)
            engine.stop()
        }
    }
}

/// Plays rendered speech as it arrives.
private final class SpeechPlayer {
    private let engine = AVAudioEngine()
    private let node = AVAudioPlayerNode()
    private var format: AVAudioFormat?
    private var attached = false
    /// The rate the node is connected at. A buffer scheduled at any
    /// other rate plays at that rate, so a changed one reconnects first.
    private var connected: AVAudioFormat?
    /// An odd trailing byte, because a frame can split a sample in half.
    private var partial = Data()
    /// Where the user wants answers. Asked for again with each answer,
    /// because the system drops an override on a route change.
    private var wantsSpeaker = true
    /// Set when the engine's graph is torn down under us, which a
    /// session or route change does. Written from the notification's own
    /// thread, so it is read and written under the lock.
    private var needsReconnect = false
    private let flag = NSLock()
    private var configurationWatch: NSObjectProtocol?

    init() {
        configurationWatch = NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange,
            object: engine,
            queue: nil
        ) { [weak self] _ in
            // A connection made before this no longer carries audio, and
            // the engine has stopped. Reconnecting is the documented
            // recovery, and without it every later answer is silent.
            self?.markStale()
        }
    }

    deinit {
        if let configurationWatch {
            NotificationCenter.default.removeObserver(configurationWatch)
        }
    }

    private func markStale() {
        flag.lock()
        needsReconnect = true
        flag.unlock()
    }

    private func takeStale() -> Bool {
        flag.lock()
        let stale = needsReconnect
        needsReconnect = false
        flag.unlock()
        return stale
    }

    func begin(codec: String, sampleRate: Double) {
        guard codec == pcm16Codec else { return }
        partial = Data()
        let rate = sampleRate > 0 ? sampleRate : 24_000
        guard
            let format = AVAudioFormat(
                commonFormat: .pcmFormatFloat32,
                sampleRate: rate,
                channels: 1,
                interleaved: false
            )
        else { return }
        self.format = format
        let stale = takeStale()
        if !attached {
            engine.attach(node)
            engine.connect(node, to: engine.mainMixerNode, format: format)
            connected = format
            attached = true
        } else if stale || connected?.sampleRate != format.sampleRate {
            // Reconnecting costs about 3 dB through the mixer and does
            // not recover for the life of the process, so it is done
            // only when the graph is stale or the rate has changed.
            node.stop()
            engine.disconnectNodeOutput(node)
            engine.connect(node, to: engine.mainMixerNode, format: format)
            connected = format
        }
        do {
            try VoiceSession.configure(mode: .voiceChat, toSpeaker: wantsSpeaker)
            if !engine.isRunning {
                try engine.start()
            }
            node.play()
        } catch {
            // Nothing to play through. The text already reached the UI.
        }
    }

    func push(_ frame: Data) {
        guard let format else { return }
        var bytes = partial
        bytes.append(frame)
        let whole = bytes.count - (bytes.count % 2)
        partial = whole == bytes.count ? Data() : bytes.subdata(in: whole..<bytes.count)
        guard whole > 0 else { return }
        let samples = whole / 2
        guard
            let buffer = AVAudioPCMBuffer(
                pcmFormat: format,
                frameCapacity: AVAudioFrameCount(samples)
            ),
            let channel = buffer.floatChannelData?[0]
        else { return }
        buffer.frameLength = AVAudioFrameCount(samples)
        bytes.withUnsafeBytes { raw in
            for i in 0..<samples {
                let lo = UInt16(raw[i * 2])
                let hi = UInt16(raw[i * 2 + 1])
                let value = Int16(bitPattern: lo | (hi << 8))
                channel[i] = Float(value) / int16FullScale
            }
        }
        // Scheduling appends to the node's own queue, so frames play back
        // to back without the caller tracking a clock.
        node.scheduleBuffer(buffer, completionHandler: nil)
    }

    func end() {
        partial = Data()
    }

    /// Stops what is playing. The engine is left running, because
    /// starting it is most of what made the first sound late.
    func stop() {
        partial = Data()
        node.stop()
    }

    /// Where the user wants answers to come out, which every answer asks
    /// for again because the system drops an override on a route change.
    func prefer(speaker: Bool) {
        wantsSpeaker = speaker
        VoiceSession.toSpeaker(speaker)
    }

    /// Readies the session and the engine before there is anything to
    /// say, so the first answer does not pay for starting them.
    func warm() {
        let ready = format ?? AVAudioFormat(
            commonFormat: .pcmFormatFloat32,
            sampleRate: 24_000,
            channels: 1,
            interleaved: false
        )
        guard let ready else { return }
        format = ready
        do {
            try VoiceSession.configure(mode: .voiceChat, toSpeaker: wantsSpeaker)
            if !attached {
                engine.attach(node)
                engine.connect(node, to: engine.mainMixerNode, format: ready)
                connected = ready
                attached = true
            }
            if !engine.isRunning {
                try engine.start()
            }
        } catch {
            // Nothing readied. The first answer tries again.
        }
    }

    /// Gives up the engine and the session, which is what stops the
    /// recording indicator once the user leaves.
    func release() {
        stop()
        if engine.isRunning {
            engine.stop()
        }
        VoiceSession.release()
    }
}
