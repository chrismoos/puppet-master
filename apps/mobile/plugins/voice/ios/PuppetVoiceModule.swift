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

/// The one audio engine, used for both the microphone and the speaker.
///
/// Apple's echo canceller lives in an engine's voice-processing unit and
/// cancels only what that engine plays, so recording on one engine while
/// speaking on another leaves it nothing to cancel against and a call on
/// the speaker hears its own answer back.
///
/// One engine at a time, but not one for the life of the process:
/// switching the canceller means building another engine. Voice
/// processing cannot be switched on a unit that has already been
/// initialized — `setVoiceProcessingEnabled` answers
/// `kAudioUnitErr_Initialized`, -10849 — and nothing un-initializes
/// one. `stop()` leaves the unit initialized, and `prepare()` and
/// `start()` are what initialize it, so readying playback when the voice
/// screen opens is on its own enough to make every later switch throw.
/// A graph that toggles in place is one whose canceller silently never
/// comes on. So each toggle builds an engine, sets the flag on it before
/// anything prepares it, and leaves whoever holds a node on the old one
/// to attach to the new one.
private final class VoiceGraph {
    static let shared = VoiceGraph()

    /// The engine in use and the toggle that produced it. Written when
    /// the canceller is switched, which runs on the module's async
    /// queue, and read by playback on the JavaScript thread, so both are
    /// held under the lock and handed out together.
    private var live = AVAudioEngine()
    private var gen = 0
    /// Set when the engine's graph is torn down under us, which a
    /// session change, a route change and the canceller being switched
    /// all do. Written from the notification's own thread, so it is read
    /// and written under the lock.
    private var stale = false
    private let flag = NSLock()
    private var watch: NSObjectProtocol?

    private init() {
        observe()
    }

    deinit {
        if let watch {
            NotificationCenter.default.removeObserver(watch)
        }
    }

    /// The engine to use, with the generation it belongs to. Read as one
    /// value, because a toggle replaces both and a caller that read them
    /// apart could attach a node to one engine and record the other.
    func current() -> (engine: AVAudioEngine, generation: Int) {
        flag.lock()
        defer { flag.unlock() }
        return (live, gen)
    }

    func markStale() {
        flag.lock()
        stale = true
        flag.unlock()
    }

    func takeStale() -> Bool {
        flag.lock()
        let was = stale
        stale = false
        flag.unlock()
        return was
    }

    /// Watches the engine in use. The notification is registered against
    /// one engine object, so a replacement would otherwise leave the
    /// registration listening to an engine nobody plays through.
    private func observe() {
        if let watch {
            NotificationCenter.default.removeObserver(watch)
        }
        watch = NotificationCenter.default.addObserver(
            forName: .AVAudioEngineConfigurationChange,
            object: current().engine,
            queue: nil
        ) { [weak self] _ in
            self?.markStale()
        }
    }

    /// Turns the echo canceller on or off, by building the engine it
    /// applies to. The switch is only accepted before anything prepares
    /// or starts the engine, so it is made on an engine nothing has
    /// touched yet and that engine becomes the one in use.
    func canceller(_ on: Bool) {
        let old = current().engine
        guard old.inputNode.isVoiceProcessingEnabled != on else { return }
        if old.isRunning {
            old.stop()
        }
        let next = AVAudioEngine()
        do {
            if next.inputNode.isVoiceProcessingEnabled != on {
                try next.inputNode.setVoiceProcessingEnabled(on)
            }
            if next.outputNode.isVoiceProcessingEnabled != on {
                try next.outputNode.setVoiceProcessingEnabled(on)
            }
        } catch {
            // Left as it was, on the engine already in use. The
            // canceller is an improvement on what the microphone hears,
            // not a requirement for hearing it.
            return
        }
        flag.lock()
        live = next
        gen += 1
        // Everything connected to the engine just replaced has to be
        // built again on this one.
        stale = true
        flag.unlock()
        observe()
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
        AsyncFunction("startListening") { (inCall: Bool) in
            try self.recognizer.start(inCall: inCall) { [weak self] text, isFinal in
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

        /// Sends output to the handset's speaker, or hands the choice
        /// back to the system, which is what picks headphones or a car.
        Function("setOutputToSpeaker") { (on: Bool) in
            VoiceSession.toSpeaker(on)
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
private final class DictationRecognizer {
    private let graph = VoiceGraph.shared
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
    /// The engine carrying the microphone tap, or nil for a microphone
    /// that has not been opened. Reaching for an engine's input node
    /// without the permission to record raises inside the engine, so
    /// nothing touches it until a tap is on, and the tap comes off the
    /// engine it went on rather than whichever is current by then.
    private var tappedEngine: AVAudioEngine?
    /// Which utterance a task belongs to. A task cancelled part way
    /// through still reports once more, and that report carries words
    /// that have already been sent. Recognition reports on a queue of
    /// its own, so this is read and written under the lock.
    private var utterance = 0
    private let turn = NSLock()
    private var onTranscript: ((String, Bool) -> Void)?
    private var onError: ((String) -> Void)?

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
        committed = ""
        restarts = 0
        listening = true
        let mine = beginUtterance()

        // Voice chat for a call, where the speaker is live while the
        // microphone is open, and measurement for tap to talk, whose
        // processing-off input recognizes better.
        try VoiceSession.configure(mode: inCall ? .voiceChat : .measurement)
        // The canceller for a call only: it takes the answer being read
        // out back off the microphone, at some cost to recognition and
        // to what playback sounds like through it.
        graph.canceller(inCall)

        let engine = graph.current().engine
        let input = engine.inputNode
        // Prepared first, because the canceller changes the input format
        // and a tap installed at the old one is refused.
        engine.prepare()
        let format = input.outputFormat(forBus: 0)
        // Appends to whichever request is current rather than to the one
        // installed with: a finalized request is replaced while the tap
        // stays where it is, and audio into a dead one is heard by
        // nobody.
        input.installTap(onBus: 0, bufferSize: 1024, format: format) { [weak self] buffer, _ in
            self?.request?.append(buffer)
        }
        tappedEngine = engine
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
        // The tap goes, the engine stays: it is the same engine that
        // plays, and an answer may still be being read out. Switching
        // the canceller off does replace it, and playback is told to
        // rebuild on the replacement rather than being stopped.
        if let tapped = tappedEngine {
            tapped.inputNode.removeTap(onBus: 0)
            tappedEngine = nil
            graph.canceller(false)
        }
    }
}

/// Plays rendered speech as it arrives.
private final class SpeechPlayer {
    private let graph = VoiceGraph.shared
    /// Replaced along with the engine: a node attached to one engine
    /// cannot be attached to another, and the engine this was attached
    /// to is thrown away whenever the canceller is switched.
    private var node = AVAudioPlayerNode()
    private var format: AVAudioFormat?
    /// The graph generation `node` is attached to, nil until it is
    /// attached to anything.
    private var attachedAt: Int?
    /// The rate the node is connected at. A buffer scheduled at any
    /// other rate plays at that rate, so a changed one reconnects first.
    private var connected: AVAudioFormat?
    /// An odd trailing byte, because a frame can split a sample in half.
    private var partial = Data()

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
        // A connection made before the graph went stale no longer
        // carries audio, so reconnecting is what keeps a later answer
        // from being silent.
        let stale = graph.takeStale()
        let (engine, generation) = graph.current()
        if attachedAt != generation {
            // Another engine. The node belongs to the one being
            // discarded, so this takes a new node on the new engine
            // rather than reconnecting the old one.
            attach(to: engine, at: generation, format: format)
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
            try VoiceSession.configure(mode: .voiceChat, toSpeaker: true)
            if !engine.isRunning {
                try engine.start()
            }
            node.play()
        } catch {
            // Nothing to play through. The text already reached the UI.
        }
    }

    /// Puts a fresh player node on an engine, connected to its mixer.
    private func attach(to engine: AVAudioEngine, at generation: Int, format: AVAudioFormat) {
        let fresh = AVAudioPlayerNode()
        engine.attach(fresh)
        engine.connect(fresh, to: engine.mainMixerNode, format: format)
        node = fresh
        connected = format
        attachedAt = generation
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
        let (engine, generation) = graph.current()
        do {
            try VoiceSession.configure(mode: .voiceChat, toSpeaker: true)
            if attachedAt != generation {
                attach(to: engine, at: generation, format: ready)
            }
            if !engine.isRunning {
                try engine.start()
            }
        } catch {
            // Nothing readied. The first answer tries again.
        }
    }

    /// Gives up the engine and the session, which is what stops the
    /// recording indicator once the user leaves. Safe to leave the
    /// microphone's tap to the recognizer, which removes it before this
    /// runs.
    func release() {
        stop()
        let engine = graph.current().engine
        if engine.isRunning {
            engine.stop()
        }
        VoiceSession.release()
    }
}
