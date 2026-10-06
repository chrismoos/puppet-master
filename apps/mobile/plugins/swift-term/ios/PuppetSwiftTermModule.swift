import ExpoModulesCore
import UIKit

public final class PuppetSwiftTermView: ExpoView, TerminalViewDelegate, UIGestureRecognizerDelegate {
    private let terminalView: PuppetTerminalView
    private let veilView = UIView()
    private let dismissKeyboardTap = UITapGestureRecognizer()

    let onInput = EventDispatcher()
    let onResize = EventDispatcher()
    let onScroll = EventDispatcher()
    let onTitle = EventDispatcher()
    let onLink = EventDispatcher()

    public required init(appContext: AppContext? = nil) {
        var options = TerminalOptions.default
        options.cursorStyle = .steadyBlock
        terminalView = PuppetTerminalView(frame: .zero, options: options)
        super.init(appContext: appContext)

        clipsToBounds = true
        backgroundColor = .black
        terminalView.translatesAutoresizingMaskIntoConstraints = false
        terminalView.terminalDelegate = self
        terminalView.allowMouseReporting = true
        terminalView.optionAsMetaKey = true
        terminalView.changeScrollback(10_000)
        terminalView.inputAccessoryView = nil
        terminalView.bounces = false
        terminalView.alwaysBounceVertical = false

        dismissKeyboardTap.addTarget(self, action: #selector(dismissKeyboardIfVisible))
        dismissKeyboardTap.cancelsTouchesInView = false
        dismissKeyboardTap.delegate = self
        terminalView.addGestureRecognizer(dismissKeyboardTap)
        for gesture in terminalView.gestureRecognizers ?? [] {
            guard let tap = gesture as? UITapGestureRecognizer, tap !== dismissKeyboardTap else { continue }
            if tap.numberOfTapsRequired > 1 {
                dismissKeyboardTap.require(toFail: tap)
            }
        }
        addSubview(terminalView)
        NSLayoutConstraint.activate([
            terminalView.leadingAnchor.constraint(equalTo: leadingAnchor),
            terminalView.trailingAnchor.constraint(equalTo: trailingAnchor),
            terminalView.topAnchor.constraint(equalTo: topAnchor),
            terminalView.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])

        veilView.backgroundColor = .black
        veilView.isHidden = false
        veilView.translatesAutoresizingMaskIntoConstraints = false
        addSubview(veilView)
        NSLayoutConstraint.activate([
            veilView.leadingAnchor.constraint(equalTo: leadingAnchor),
            veilView.trailingAnchor.constraint(equalTo: trailingAnchor),
            veilView.topAnchor.constraint(equalTo: topAnchor),
            veilView.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    @objc private func dismissKeyboardIfVisible() {
        if terminalView.isFirstResponder {
            terminalView.resignFirstResponder()
        }
    }

    public override func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
        gestureRecognizer !== dismissKeyboardTap || terminalView.isFirstResponder
    }

    public func gestureRecognizer(
        _ gestureRecognizer: UIGestureRecognizer,
        shouldRecognizeSimultaneouslyWith otherGestureRecognizer: UIGestureRecognizer
    ) -> Bool {
        gestureRecognizer === dismissKeyboardTap || otherGestureRecognizer === dismissKeyboardTap
    }

    func write(base64: String) throws {
        guard let data = Data(base64Encoded: base64) else {
            throw InvalidTerminalDataException(base64)
        }
        // SwiftTerm auto-scrolls to the bottom on every feed. When the
        // user has scrolled up to read earlier output, pin the viewport
        // so new arrivals do not yank it away.
        let maxY = max(0, terminalView.contentSize.height - terminalView.bounds.height)
        let atBottom = maxY <= 0 || terminalView.contentOffset.y >= maxY - 1
        let savedOffset = terminalView.contentOffset
        terminalView.feed(byteArray: [UInt8](data)[...])
        if !atBottom {
            terminalView.setContentOffset(savedOffset, animated: false)
        }
    }

    func focusTerminal() {
        terminalView.becomeFirstResponder()
    }

    func blurTerminal() {
        terminalView.resignFirstResponder()
    }

    func clearTerminalSelection() {
        terminalView.clearSelection()
    }

    func scrollToBottom() {
        terminalView.scroll(toPosition: 1)
    }

    func setVeiled(_ veiled: Bool) {
        if veiled {
            veilView.layer.removeAllAnimations()
            veilView.alpha = 1
            veilView.isHidden = false
        } else {
            UIView.animate(withDuration: 0.05, animations: {
                self.veilView.alpha = 0
            }, completion: { _ in
                self.veilView.isHidden = true
            })
        }
    }

    func revealAtBottom(promise: Promise) {
        terminalView.scroll(toPosition: 1)
        terminalView.layoutIfNeeded()
        let link = CADisplayLink(target: DisplayLinkTrampoline {
            self.veilView.layer.removeAllAnimations()
            UIView.animate(withDuration: 0.05, animations: {
                self.veilView.alpha = 0
            }, completion: { _ in
                self.veilView.isHidden = true
            })
            promise.resolve()
        }, selector: #selector(DisplayLinkTrampoline.fire))
        link.add(to: .main, forMode: .common)
    }

    func configure(fontSize: Double, foreground: UIColor?, background: UIColor?, selection: UIColor?) {
        terminalView.font = UIFont.monospacedSystemFont(ofSize: fontSize, weight: .regular)
        if let foreground { terminalView.nativeForegroundColor = foreground }
        if let background {
            terminalView.nativeBackgroundColor = background
            terminalView.backgroundColor = background
            self.backgroundColor = background
            veilView.backgroundColor = background
        }
        if let selection { terminalView.selectedTextBackgroundColor = selection }
    }

    public func send(source: TerminalView, data: ArraySlice<UInt8>) {
        onInput(["dataBase64": Data(data).base64EncodedString()])
    }

    public func sizeChanged(source: TerminalView, newCols: Int, newRows: Int) {
        onResize(["cols": newCols, "rows": newRows])
    }

    public func scrolled(source: TerminalView, position: Double) {
        onScroll(["position": position])
    }

    public func setTerminalTitle(source: TerminalView, title: String) {
        onTitle(["title": title])
    }

    public func requestOpenLink(source: TerminalView, link: String, params: [String: String]) {
        onLink(["url": link])
    }

    public func hostCurrentDirectoryUpdate(source: TerminalView, directory: String?) {}
    public func bell(source: TerminalView) {}
    public func clipboardCopy(source: TerminalView, content: Data) {}
    public func clipboardRead(source: TerminalView) -> Data? { nil }
    public func iTermContent(source: TerminalView, content: ArraySlice<UInt8>) {}
    public func rangeChanged(source: TerminalView, startY: Int, endY: Int) {}
}

public final class PuppetSwiftTermModule: Module {
    public func definition() -> ModuleDefinition {
        Name("PuppetSwiftTerm")

        View(PuppetSwiftTermView.self) {
            Events("onInput", "onResize", "onScroll", "onTitle", "onLink")

            Prop("fontSize") { (view, value: Double) in
                view.configure(fontSize: value, foreground: nil, background: nil, selection: nil)
            }

            Prop("foregroundColor") { (view, value: UIColor?) in
                view.configure(fontSize: Double(view.terminalFontSize), foreground: value, background: nil, selection: nil)
            }

            Prop("backgroundColor") { (view, value: UIColor?) in
                view.configure(fontSize: Double(view.terminalFontSize), foreground: nil, background: value, selection: nil)
            }

            Prop("selectionColor") { (view, value: UIColor?) in
                view.configure(fontSize: Double(view.terminalFontSize), foreground: nil, background: nil, selection: value)
            }

            Prop("veiled") { (view, value: Bool) in
                view.setVeiled(value)
            }

            AsyncFunction("write") { (view: PuppetSwiftTermView, base64: String) in
                try view.write(base64: base64)
            }

            AsyncFunction("focus") { (view: PuppetSwiftTermView) in view.focusTerminal() }
            AsyncFunction("blur") { (view: PuppetSwiftTermView) in view.blurTerminal() }
            AsyncFunction("clearSelection") { (view: PuppetSwiftTermView) in view.clearTerminalSelection() }
            AsyncFunction("scrollToBottom") { (view: PuppetSwiftTermView) in view.scrollToBottom() }

            AsyncFunction("revealAtBottom") { (view: PuppetSwiftTermView, promise: Promise) in
                view.revealAtBottom(promise: promise)
            }
        }
    }
}

private final class DisplayLinkTrampoline: NSObject {
    private let action: () -> Void

    init(_ action: @escaping () -> Void) {
        self.action = action
    }

    @objc func fire(_ link: CADisplayLink) {
        link.invalidate()
        action()
    }
}

private final class InvalidTerminalDataException: GenericException<String> {
    override var reason: String { "Invalid base64 terminal data" }
}

private extension PuppetSwiftTermView {
    var terminalFontSize: CGFloat { terminalView.font.pointSize }
}

// MARK: - Swipe-to-scroll terminal view

/// UIPanGestureRecognizer that records the raw touch-down timestamp so the
/// handler can distinguish a quick swipe from a long-press drag.
private final class TimedPanGestureRecognizer: UIPanGestureRecognizer {
    private(set) var touchDownTime: TimeInterval = 0

    override func touchesBegan(_ touches: Set<UITouch>, with event: UIEvent) {
        super.touchesBegan(touches, with: event)
        touchDownTime = CACurrentMediaTime()
    }

    override func reset() {
        super.reset()
        touchDownTime = 0
    }
}

/// Subclass of SwiftTerm's TerminalView that replaces the default mouse-pan
/// gesture with one that distinguishes quick vertical swipes from long-press
/// drags.
///
/// SwiftTerm's built-in `panMouseHandler` sends mouse button 0
/// press/motion/release for every pan when mouse tracking is on. Fullscreen
/// TUIs (Claude Code, vim, etc.) interpret that as a text selection, which is
/// wrong for a quick scroll gesture. This subclass intercepts the pan and:
///
///  - Quick swipe (touch-to-drag < 500 ms): sends mouse wheel events
///    (button 64 up / 65 down) on the alternate screen, or scrolls the
///    viewport on the normal screen.
///  - Long-press drag (≥ 500 ms): sends press/motion/release so the TUI
///    can perform mouse-based selection.
final class PuppetTerminalView: TerminalView {
    private var wheelPanGesture: TimedPanGestureRecognizer?

    private enum PanMode { case undecided, scroll, mouseDrag }
    private var panMode: PanMode = .undecided
    private var accumPx: CGFloat = 0
    private var lastPanY: CGFloat = 0
    private var dragStarted = false

    private static let longPressSec: TimeInterval = 0.5

    // MARK: Mouse-mode lifecycle

    override func mouseModeChanged(source: Terminal) {
        // Do NOT call super — that would install SwiftTerm's panMouseGesture
        // which sends drag events for every pan.
        reconcileWheelPan(source: source)
    }

    override func bufferActivated(source: Terminal) {
        super.bufferActivated(source: source)
        reconcileWheelPan(source: source)
    }

    /// Install the wheel-pan gesture whenever the alternate screen is active
    /// (for keyboard-scroll translation) or mouse mode is on (for wheel events).
    /// Uses the callback's `source` parameter because `self.terminal` is nil
    /// during Terminal.init when mouseModeChanged first fires.
    private func reconcileWheelPan(source: Terminal) {
        if source.mouseMode != .off || source.isCurrentBufferAlternate {
            installWheelPan()
        } else {
            removeWheelPan()
        }
    }

    private func installWheelPan() {
        disableMousePanGesture()          // Remove SwiftTerm's default if present
        guard wheelPanGesture == nil else { return }
        let gesture = TimedPanGestureRecognizer(target: self, action: #selector(handleWheelPan))
        addGestureRecognizer(gesture)
        wheelPanGesture = gesture
    }

    private func removeWheelPan() {
        if let g = wheelPanGesture {
            removeGestureRecognizer(g)
            wheelPanGesture = nil
        }
    }

    // MARK: Pan handler

    @objc private func handleWheelPan(_ recognizer: TimedPanGestureRecognizer) {
        guard recognizer.view != nil else { return }

        let point = recognizer.location(in: self)

        switch recognizer.state {
        case .began:
            let elapsed = CACurrentMediaTime() - recognizer.touchDownTime
            lastPanY = point.y
            accumPx = 0
            dragStarted = false
            // Without mouse mode there is no drag target, so every swipe
            // scrolls regardless of hold duration.
            if terminal.mouseMode == .off || elapsed < Self.longPressSec {
                panMode = .scroll
            } else {
                panMode = .mouseDrag
                beginMouseDrag(at: point)
            }

        case .changed:
            switch panMode {
            case .undecided:
                break
            case .scroll:
                accumPx += point.y - lastPanY
                lastPanY = point.y
                flushWheelEvents(at: point)
            case .mouseDrag:
                emitMouseMotion(at: point)
            }

        case .ended:
            if panMode == .mouseDrag && dragStarted {
                emitMouseRelease(at: point)
            }
            panMode = .undecided
            dragStarted = false

        case .cancelled:
            if panMode == .mouseDrag && dragStarted {
                emitMouseRelease(at: point)
            }
            panMode = .undecided
            dragStarted = false

        default:
            break
        }
    }

    // MARK: Wheel emission

    private func flushWheelEvents(at point: CGPoint) {
        let lineHeight = cellDimension.height
        guard lineHeight > 0 else { return }

        let lines = Int(accumPx / lineHeight)
        guard lines != 0 else { return }
        accumPx -= CGFloat(lines) * lineHeight

        if terminal.isCurrentBufferAlternate {
            if terminal.mouseMode != .off {
                let hit = calculateTapHit(point: point)
                guard let screen = hit.grid.toScreenCoordinate(from: terminal.displayBuffer) else { return }
                // Finger down → content up → wheel-up (64); finger up → wheel-down (65)
                let buttonFlags = lines > 0 ? 64 : 65
                for _ in 0..<abs(lines) {
                    terminal.sendEvent(buttonFlags: buttonFlags,
                                       x: screen.col, y: screen.row,
                                       pixelX: hit.pixels.col, pixelY: hit.pixels.row)
                }
            } else {
                // TUI without mouse mode (e.g. OpenCode): translate swipes
                // to arrow keys so the application can scroll its viewport.
                // Finger down → content up → arrow up; finger up → arrow down.
                let key: UInt8 = lines > 0 ? 0x41 : 0x42 // A = up, B = down
                let seq: [UInt8] = [0x1b, 0x5b, key]     // ESC [ A / ESC [ B
                for _ in 0..<abs(lines) {
                    terminal.sendUserInput(seq[...])
                }
            }
        } else {
            scrollDown(lines: -lines)
        }
    }

    // MARK: Mouse drag (long-press selection)

    private func beginMouseDrag(at point: CGPoint) {
        guard terminal.mouseMode.sendButtonPress() else { return }
        dragStarted = true
        let hit = calculateTapHit(point: point)
        if let screen = hit.grid.toScreenCoordinate(from: terminal.displayBuffer) {
            terminal.sendEvent(buttonFlags: encodeFlags(release: false),
                               x: screen.col, y: screen.row,
                               pixelX: hit.pixels.col, pixelY: hit.pixels.row)
        }
    }

    private func emitMouseMotion(at point: CGPoint) {
        guard terminal.mouseMode.sendButtonTracking() else { return }
        let hit = calculateTapHit(point: point)
        if let screen = hit.grid.toScreenCoordinate(from: terminal.displayBuffer) {
            terminal.sendMotion(buttonFlags: encodeFlags(release: false),
                                x: screen.col, y: screen.row,
                                pixelX: hit.pixels.col, pixelY: hit.pixels.row)
        }
    }

    private func emitMouseRelease(at point: CGPoint) {
        guard terminal.mouseMode.sendButtonRelease() else { return }
        let hit = calculateTapHit(point: point)
        if let screen = hit.grid.toScreenCoordinate(from: terminal.displayBuffer) {
            terminal.sendEvent(buttonFlags: encodeFlags(release: true),
                               x: screen.col, y: screen.row,
                               pixelX: hit.pixels.col, pixelY: hit.pixels.row)
        }
    }
}
