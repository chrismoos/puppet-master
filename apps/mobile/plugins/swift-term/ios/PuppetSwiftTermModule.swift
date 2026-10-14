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
    let onSelection = EventDispatcher()
    let onAppScroll = EventDispatcher()

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
        terminalView.installTouchSelection()
        terminalView.onSelectionChange = { [weak self] active in
            self?.onSelection(["active": active])
        }
        terminalView.onViewportMoved = { [weak self] in
            guard let self, !self.feeding else { return }
            self.reportScrollPosition()
        }
        terminalView.onAppScrolledChange = { [weak self] up in
            self?.onAppScroll(["up": up])
        }
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
        guard gestureRecognizer === dismissKeyboardTap else { return true }
        // A tap on a link opens it and a tap on a selection clears it;
        // neither should also drop the keyboard.
        return terminalView.isFirstResponder
            && !terminalView.selection.active
            && terminalView.openableLink(at: gestureRecognizer.location(in: terminalView)) == nil
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
        var savedOffset = terminalView.contentOffset
        let buffer = terminalView.getTerminal().buffer
        let trimmedBefore = buffer.totalLinesTrimmed
        feeding = true
        terminalView.feed(byteArray: [UInt8](data)[...])
        if !atBottom {
            // A full scrollback drops lines off the top without growing, so
            // the same pixel offset would show later text.
            let trimmed = terminalView.getTerminal().buffer === buffer
                ? max(0, buffer.totalLinesTrimmed - trimmedBefore) : 0
            savedOffset.y = max(0, savedOffset.y - CGFloat(trimmed) * terminalView.cellDimension.height)
            terminalView.setContentOffset(savedOffset, animated: false)
        }
        feeding = false
        reportScrollPosition()
    }

    /// True while a write is being fed: SwiftTerm reports the position
    /// mid-feed (Claude Code ends every frame's synchronized output, which
    /// reports it) from its model, already back at the bottom, before the
    /// viewport is pinned again. Those reports hid the jump button.
    private var feeding = false
    private var reportedAtBottom: Bool?

    /// Reports where the viewport actually is, from what is on screen.
    private func reportScrollPosition() {
        let terminal = terminalView.getTerminal()
        let buffer = terminal.displayBuffer
        let position: Double
        // Neither the alternate screen nor a screen without scrollback has
        // local scrollback to leave (SwiftTerm says 0 for both).
        if terminal.isDisplayBufferAlternate || buffer.lines.count <= buffer.rows {
            position = 1
        } else {
            // SwiftTerm's own bottom: inset included, within half a row.
            let view = terminalView
            let maxY = view.contentSize.height - view.bounds.height + view.adjustedContentInset.bottom
            let y = view.contentOffset.y
            // Off the bottom stays clearly below 1: with a long scrollback a
            // few rows up is 0.9999, which the shell reads as following.
            position = maxY <= 0 || y >= maxY - view.cellDimension.height / 2 ? 1 : min(0.99, max(0, y / maxY))
        }
        // Only the at-bottom edge matters to the jump button; skip the
        // stream of positions a drag or fling would otherwise send.
        let atBottom = position >= 1
        guard atBottom != reportedAtBottom else { return }
        reportedAtBottom = atBottom
        onScroll(["position": position])
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

    func copyTerminalSelection() {
        terminalView.copySelection()
    }

    func scrollToBottom(ctrlEnd: Bool) {
        if terminalView.scrollAppToBottom(ctrlEnd: ctrlEnd) { return }
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
        guard !feeding else { return }
        reportScrollPosition()
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
            Events("onInput", "onResize", "onScroll", "onTitle", "onLink", "onSelection", "onAppScroll")

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
            AsyncFunction("copySelection") { (view: PuppetSwiftTermView) in view.copyTerminalSelection() }
            AsyncFunction("scrollToBottom") { (view: PuppetSwiftTermView, ctrlEnd: Bool) in
                view.scrollToBottom(ctrlEnd: ctrlEnd)
            }

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

/// Subclass of SwiftTerm's TerminalView tuned for touch.
///
/// SwiftTerm's built-in `panMouseHandler` sends mouse button 0
/// press/motion/release for every pan when mouse tracking is on. Fullscreen
/// TUIs (Claude Code, vim, etc.) interpret that as a text selection, which is
/// wrong for a scroll gesture, so a swipe here sends mouse wheel events
/// (button 64 up / 65 down) on the alternate screen, or scrolls the viewport
/// on the normal screen.
///
/// Text selection is local and owned by this view rather than the TUI: a
/// long press selects the word under the finger and dragging on extends it,
/// whatever mouse mode the application asked for. See `installTouchSelection`.
final class PuppetTerminalView: TerminalView, UIEditMenuInteractionDelegate {
    private var wheelPanGesture: UIPanGestureRecognizer?
    private var accumPx: CGFloat = 0
    private var lastPanY: CGFloat = 0

    private static let selectionPressSec: TimeInterval = 0.4

    /// Reports when a local selection appears or goes away.
    var onSelectionChange: ((Bool) -> Void)?
    private var reportedSelection = false

    /// Reports when a full-screen app is scrolled up inside itself, or back.
    var onAppScrolledChange: ((Bool) -> Void)?
    /// Called whenever the viewport moves, by a drag, a fling or code.
    /// SwiftTerm reports its scroll position only when the terminal itself
    /// scrolls, so a reader scrolling idle scrollback went unnoticed.
    var onViewportMoved: (() -> Void)?

    override var contentOffset: CGPoint {
        didSet { onViewportMoved?() }
    }

    /// Net lines swiped up on the alternate screen, which the app scrolled
    /// itself and the local scrollback cannot see.
    private var appScrollLines = 0
    private lazy var editMenu = UIEditMenuInteraction(delegate: self)
    private lazy var handlePan = UIPanGestureRecognizer(target: self, action: #selector(dragSelectionHandle))
    private let handlePanGate = HandlePanGate()

    // MARK: Keyboard

    /// Always claims text, as terminal apps do: the system keyboard only
    /// auto-repeats a held delete key while the input reports text to
    /// delete, and SwiftTerm's input buffer is empty almost always, so
    /// holding delete removed a single character.
    override var hasText: Bool { true }

    // MARK: Link taps

    /// The link under a point, if tapping it should open it.
    ///
    /// SwiftTerm opens a tapped link only once hovering has highlighted it,
    /// and a finger never hovers, so the link is looked up directly here. An
    /// explicit OSC 8 hyperlink always counts. Text that merely looks like a
    /// link counts only with a web scheme, so a tap on a file path keeps its
    /// ordinary meaning.
    func openableLink(at point: CGPoint) -> String? {
        let position = calculateTapHit(point: point).grid
        guard let match = terminal.linkMatch(at: .buffer(position), mode: .explicitAndImplicit) else {
            return nil
        }
        if match.isExplicit {
            guard let payload = payloadString(at: position),
                  let (url, _) = urlAndParamsFrom(payload: payload) else { return nil }
            return url
        }
        let lower = match.text.lowercased()
        return lower.hasPrefix("https://") || lower.hasPrefix("http://") ? match.text : nil
    }

    @objc override func singleTap(_ gestureRecognizer: UITapGestureRecognizer) {
        guard gestureRecognizer.state == .ended else { return }
        // The first tap after selecting only dismisses the selection, so
        // tapping away from it never opens a link or reaches the TUI.
        if selection.active {
            clearSelection()
            requestDisplay()
            return
        }
        if let url = openableLink(at: gestureRecognizer.location(in: self)) {
            terminalDelegate?.requestOpenLink(source: self, link: url, params: [:])
            return
        }
        super.singleTap(gestureRecognizer)
    }

    // MARK: Touch selection

    /// Retunes SwiftTerm's long press into the selection gesture.
    ///
    /// SwiftTerm's own long press opened a Select/Paste menu that took
    /// first responder (raising the keyboard, whose resize clears any
    /// selection), and it cleared the selection on every chunk of output
    /// while mouse reporting was on, which is always for agent TUIs that
    /// redraw constantly. So the press selects directly here, and mouse
    /// reporting is paused for as long as a selection exists.
    func installTouchSelection() {
        for case let press as UILongPressGestureRecognizer in gestureRecognizers ?? [] {
            press.minimumPressDuration = Self.selectionPressSec
        }
        addInteraction(editMenu)
        handlePanGate.view = self
        handlePan.delegate = handlePanGate
        addGestureRecognizer(handlePan)
        panGestureRecognizer.require(toFail: handlePan)
    }

    // MARK: Selection handles

    /// The selection's ends in reading order, where the handles are drawn.
    private func orderedSelection() -> (start: Position, end: Position) {
        let a = selection.start, b = selection.end
        return Position.compare(a, b) == .before ? (a, b) : (b, a)
    }

    /// Which handle, if any, a touch at `point` grabs: the far end then
    /// stays put as the pivot while the grabbed one follows the finger.
    fileprivate func grabbedHandlePivot(at point: CGPoint) -> Position? {
        guard selection.active else { return nil }
        let (start, end) = orderedSelection()
        // Handles sit at the start's top-left and the end's bottom-left
        // corner; take the nearer one, the end on a tie (short words).
        let w = cellDimension.width, h = cellDimension.height
        let startKnob = CGPoint(x: CGFloat(start.col) * w, y: CGFloat(start.row) * h)
        let endKnob = CGPoint(x: CGFloat(end.col) * w, y: CGFloat(end.row + 1) * h)
        let toStart = hypot(point.x - startKnob.x, point.y - startKnob.y)
        let toEnd = hypot(point.x - endKnob.x, point.y - endKnob.y)
        let reach: CGFloat = 32
        if toEnd <= reach && toEnd <= toStart { return start }
        if toStart <= reach { return end }
        return nil
    }

    /// Drags a selection handle after the long press has ended, the way
    /// native text handles move; without this the drag scrolled instead.
    @objc private func dragSelectionHandle(_ recognizer: UIPanGestureRecognizer) {
        switch recognizer.state {
        case .began:
            guard let pivot = grabbedHandlePivot(at: recognizer.location(in: self)) else { return }
            editMenu.dismissMenu()
            selection.pivot = pivot
            UISelectionFeedbackGenerator().selectionChanged()
        case .changed:
            guard selection.active else { return }
            selection.pivotExtend(bufferPosition: calculateTapHit(point: recognizer.location(in: self)).grid)
            requestDisplay()
        case .ended:
            if selection.active { showSelectionMenu() }
        default:
            break
        }
    }

    private func near(_ a: Position, _ b: Position) -> Bool {
        abs(a.col - b.col) < 3 && abs(a.row - b.row) < 2
    }

    @objc override func longPress(_ gestureRecognizer: UILongPressGestureRecognizer) {
        let hit = calculateTapHit(point: gestureRecognizer.location(in: self)).grid
        switch gestureRecognizer.state {
        case .began:
            editMenu.dismissMenu()
            setScrollGesturesEnabled(false)
            allowMouseReporting = false
            if selection.active && near(selection.start, hit) {
                selection.pivot = selection.end
                selection.pivotExtend(bufferPosition: hit)
            } else if selection.active && near(selection.end, hit) {
                selection.pivot = selection.start
                selection.pivotExtend(bufferPosition: hit)
            } else {
                selection.selectWordOrExpression(at: hit, in: terminal.displayBuffer)
                selection.selectionMode = .character
                selection.pivot = selection.start
            }
            UISelectionFeedbackGenerator().selectionChanged()
            requestDisplay()
        case .changed:
            selection.pivotExtend(bufferPosition: hit)
            requestDisplay()
        case .ended:
            setScrollGesturesEnabled(true)
            if selection.active { showSelectionMenu() }
        case .cancelled, .failed:
            setScrollGesturesEnabled(true)
        default:
            break
        }
    }

    /// Stops the scroll pans while a finger drags out a selection, so the
    /// same drag does not also scroll the viewport or the TUI.
    private func setScrollGesturesEnabled(_ enabled: Bool) {
        panGestureRecognizer.isEnabled = enabled
        wheelPanGesture?.isEnabled = enabled
    }

    override func selectionChanged(source: Terminal) {
        super.selectionChanged(source: source)
        let active = selection.active
        if !active {
            // Mouse reporting resumes once nothing is selected.
            allowMouseReporting = true
            editMenu.dismissMenu()
        }
        guard active != reportedSelection else { return }
        reportedSelection = active
        onSelectionChange?(active)
    }

    func copySelection() {
        guard selection.active else { return }
        UIPasteboard.general.string = selection.getSelectedText()
        clearSelection()
        requestDisplay()
    }

    private func selectionRect() -> CGRect {
        let top = CGFloat(selection.start.row) * cellDimension.height
        let rows = CGFloat(selection.end.row - selection.start.row + 1)
        let x = selection.isMultiLine ? 0 : CGFloat(selection.start.col) * cellDimension.width
        let width = selection.isMultiLine
            ? bounds.width
            : max(cellDimension.width, CGFloat(selection.end.col - selection.start.col) * cellDimension.width)
        return CGRect(x: x, y: top, width: width, height: rows * cellDimension.height)
    }

    private func showSelectionMenu() {
        let rect = selectionRect()
        let config = UIEditMenuConfiguration(identifier: nil, sourcePoint: CGPoint(x: rect.midX, y: rect.minY))
        editMenu.presentEditMenu(with: config)
    }

    func editMenuInteraction(
        _ interaction: UIEditMenuInteraction,
        menuFor configuration: UIEditMenuConfiguration,
        suggestedActions: [UIMenuElement]
    ) -> UIMenu? {
        UIMenu(children: [
            UIAction(title: "Copy", image: UIImage(systemName: "doc.on.doc")) { [weak self] _ in
                self?.copySelection()
            },
        ])
    }

    func editMenuInteraction(
        _ interaction: UIEditMenuInteraction,
        targetRectFor configuration: UIEditMenuConfiguration
    ) -> CGRect {
        selectionRect()
    }

    // MARK: Mouse-mode lifecycle

    override func mouseModeChanged(source: Terminal) {
        // Do NOT call super — that would install SwiftTerm's panMouseGesture
        // which sends drag events for every pan.
        reconcileWheelPan(source: source)
    }

    override func bufferActivated(source: Terminal) {
        super.bufferActivated(source: source)
        setAppScrollLines(0)
        // The local scroll position changes meaning with the buffer. Async:
        // this also fires inside Terminal.init, before `terminal` is set.
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.terminalDelegate?.scrolled(source: self, position: self.scrollPosition)
        }
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
        let gesture = UIPanGestureRecognizer(target: self, action: #selector(handleWheelPan))
        gesture.require(toFail: handlePan)
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

    @objc private func handleWheelPan(_ recognizer: UIPanGestureRecognizer) {
        guard recognizer.view != nil else { return }

        let point = recognizer.location(in: self)

        switch recognizer.state {
        case .began:
            lastPanY = point.y
            accumPx = 0
        case .changed:
            accumPx += point.y - lastPanY
            lastPanY = point.y
            flushWheelEvents(at: point)
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
            emitAppScroll(lines: lines, at: point)
            // Arrow keys (no mouse mode) may be editing, not scrolling, so
            // only wheel scrolls are tracked for the jump back down.
            if terminal.mouseMode != .off {
                setAppScrollLines(max(0, appScrollLines + lines))
            }
        } else {
            scrollDown(lines: -lines)
        }
    }

    /// Scrolls a full-screen app by `lines` (positive = content up, as a
    /// finger dragged down) through wheel reports, or arrows without mouse.
    private func emitAppScroll(lines: Int, at point: CGPoint) {
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
    }

    private func setAppScrollLines(_ lines: Int) {
        let wasUp = appScrollLines > 0
        appScrollLines = lines
        if wasUp != (lines > 0) { onAppScrolledChange?(lines > 0) }
    }

    /// Returns a full-screen app scrolled up inside itself to its bottom:
    /// Ctrl+End where the app binds it (Claude Code), otherwise by undoing
    /// the swipes. False when no app scroll is pending.
    func scrollAppToBottom(ctrlEnd: Bool) -> Bool {
        guard terminal.isCurrentBufferAlternate, terminal.mouseMode != .off, appScrollLines > 0 else { return false }
        if ctrlEnd {
            send([0x1b, 0x5b, 0x31, 0x3b, 0x35, 0x46]) // ESC [ 1 ; 5 F
        } else {
            emitAppScroll(lines: -appScrollLines, at: CGPoint(x: bounds.midX, y: contentOffset.y + bounds.height / 2))
        }
        setAppScrollLines(0)
        return true
    }
}

/// Lets the handle drag start only on a touch that lands on a handle, so
/// every other drag fails it at once and scrolls as before.
private final class HandlePanGate: NSObject, UIGestureRecognizerDelegate {
    weak var view: PuppetTerminalView?

    func gestureRecognizerShouldBegin(_ gestureRecognizer: UIGestureRecognizer) -> Bool {
        guard let view, let pan = gestureRecognizer as? UIPanGestureRecognizer else { return false }
        // Where the finger first came down, not where the pan threshold tripped.
        let start = pan.location(in: view) - pan.translation(in: view)
        return view.grabbedHandlePivot(at: start) != nil
    }
}

private func - (a: CGPoint, b: CGPoint) -> CGPoint { CGPoint(x: a.x - b.x, y: a.y - b.y) }
