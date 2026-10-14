import XCTest

/// Drives the transitions that rebuild the terminal picture: opening a
/// session with deep scrollback (replay) and showing and hiding the keyboard
/// (resize). Run under a screen recording; the frames in between are what
/// the reader sees, and should go from one settled picture to the next.
final class TerminalSmoothnessTests: XCTestCase {
    private var app: XCUIApplication!

    override func setUpWithError() throws {
        continueAfterFailure = false
        app = XCUIApplication()
        app.launch()
        XCTAssertTrue(app.wait(for: .runningForeground, timeout: 10), "app did not reach foreground")
        let sb = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let allow = sb.buttons["Allow"]
        if allow.waitForExistence(timeout: 3) {
            let deny = sb.buttons["Don\u{2019}t Allow"]
            if deny.exists { deny.tap() } else { allow.tap() }
            _ = app.wait(for: .runningForeground, timeout: 3)
        }
    }

    private func mark(_ name: String) {
        // The recording is aligned to these by wall clock.
        print("SMOOTH-MARK \(name) \(Date().timeIntervalSince1970)")
    }

    private func row(_ session: String) -> XCUIElement {
        app.descendants(matching: .any).matching(
            NSPredicate(format: "label CONTAINS[c] %@", session)).firstMatch
    }

    private func showKeyboard(_ frame: XCUIElement) {
        for _ in 0..<3 where !app.keyboards.firstMatch.exists {
            frame.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.5)).tap()
            _ = app.keyboards.firstMatch.waitForExistence(timeout: 3)
        }
        XCTAssertTrue(app.keyboards.firstMatch.exists, "keyboard did not appear")
    }

    private func hideKeyboard(_ frame: XCUIElement) {
        frame.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.3)).tap()
        let gone = NSPredicate(format: "exists == false")
        XCTAssertEqual(XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: gone, object: app.keyboards.firstMatch)], timeout: 5),
            .completed, "keyboard did not hide")
    }

    func testReplayAndKeyboardTransitions() throws {
        let session = ProcessInfo.processInfo.environment["SMOOTH_SESSION"] ?? "fixture-worker"
        XCTAssertTrue(row(session).waitForExistence(timeout: 20), "\(session) not listed")
        row(session).tap()
        let frame = app.otherElements["terminal-frame"]
        XCTAssertTrue(frame.waitForExistence(timeout: 10), "terminal did not open")
        Thread.sleep(forTimeInterval: 2.0)
        if session.hasPrefix("fixture") {
            showKeyboard(frame)
            app.typeText("mouseoff\n")
            app.typeText("lineout 600\n")
            Thread.sleep(forTimeInterval: 4.0)
            hideKeyboard(frame)
            Thread.sleep(forTimeInterval: 1.0)
        }

        for round in 0..<2 {
            app.descendants(matching: .any).matching(NSPredicate(format: "label == %@", "Back to sessions")).firstMatch.tap()
            XCTAssertTrue(row(session).waitForExistence(timeout: 10))
            Thread.sleep(forTimeInterval: 1.5)
            mark("open\(round)")
            row(session).tap()
            XCTAssertTrue(frame.waitForExistence(timeout: 10))
            Thread.sleep(forTimeInterval: 3.0)
            mark("kbshow\(round)")
            showKeyboard(frame)
            Thread.sleep(forTimeInterval: 2.5)
            mark("kbhide\(round)")
            hideKeyboard(frame)
            Thread.sleep(forTimeInterval: 2.5)
        }
        mark("end")
    }

    /// Real Claude Code repaints on every resize and its scrollback is a
    /// long, wide replay: the case readers see on their own sessions.
    func testClaudeReplayAndKeyboardTransitions() throws {
        let more = app.buttons["More options"]
        XCTAssertTrue(more.waitForExistence(timeout: 20), "sessions screen not reached")
        more.tap()
        let label = { (text: String) in
            self.app.descendants(matching: .any).matching(NSPredicate(format: "label == %@", text)).firstMatch
        }
        label("New session").tap()
        let project = label("Select\u{2026}")
        if project.waitForExistence(timeout: 3) {
            project.tap()
            label("controller-fixture project").tap()
        }
        XCTAssertTrue(label("Spawn").waitForExistence(timeout: 5))
        label("Spawn").tap()
        let frame = app.otherElements["terminal-frame"]
        XCTAssertTrue(frame.waitForExistence(timeout: 20), "the spawned session did not open")
        Thread.sleep(forTimeInterval: 10.0)
        showKeyboard(frame)
        app.typeText("Print the numbers 1 to 250, one per line, as plain text in your reply. Use no tools.\n")
        Thread.sleep(forTimeInterval: 40.0)
        hideKeyboard(frame)
        Thread.sleep(forTimeInterval: 2.0)
        for round in 0..<2 {
            mark("ckbshow\(round)")
            showKeyboard(frame)
            Thread.sleep(forTimeInterval: 2.5)
            mark("ckbhide\(round)")
            hideKeyboard(frame)
            Thread.sleep(forTimeInterval: 2.5)
        }
        label("Back to sessions").tap()
        Thread.sleep(forTimeInterval: 2.0)
        print(app.debugDescription)
        let newest = app.descendants(matching: .any).matching(
            NSPredicate(format: "label BEGINSWITH %@", "Printing numbers")).firstMatch
        XCTAssertTrue(newest.waitForExistence(timeout: 5), "claude session not listed")
        for round in 0..<2 {
            mark("copen\(round)")
            newest.tap()
            XCTAssertTrue(frame.waitForExistence(timeout: 10))
            Thread.sleep(forTimeInterval: 3.5)
            label("Back to sessions").tap()
            Thread.sleep(forTimeInterval: 2.0)
        }
        mark("end")
    }

    /// Reopens the Claude session the test above spawned: a replay of real
    /// Claude Code output, then the keyboard over it.
    func testClaudeReopen() throws {
        let session = app.descendants(matching: .any).matching(
            NSPredicate(format: "label BEGINSWITH %@", "Printing numbers")).firstMatch
        XCTAssertTrue(session.waitForExistence(timeout: 20), "claude session not listed")
        let frame = app.otherElements["terminal-frame"]
        let back = app.descendants(matching: .any).matching(
            NSPredicate(format: "label == %@", "Back to sessions")).firstMatch
        for round in 0..<2 {
            Thread.sleep(forTimeInterval: 1.5)
            mark("copen\(round)")
            session.tap()
            XCTAssertTrue(frame.waitForExistence(timeout: 10))
            Thread.sleep(forTimeInterval: 3.0)
            mark("ckbshow\(round)")
            showKeyboard(frame)
            Thread.sleep(forTimeInterval: 2.5)
            mark("ckbhide\(round)")
            hideKeyboard(frame)
            Thread.sleep(forTimeInterval: 2.5)
            back.tap()
            XCTAssertTrue(session.waitForExistence(timeout: 10))
        }
        mark("end")
    }
}
