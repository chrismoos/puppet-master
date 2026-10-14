import XCTest

/// Keyboard and scrolling behavior on the test agent, which echoes the
/// argument of `echo` raw.
final class TerminalKeyboardTests: XCTestCase {
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

    override func tearDownWithError() throws {
        let att = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        att.name = "final"
        att.lifetime = .keepAlways
        add(att)
    }

    private func openTerminal(_ session: String) -> XCUIElement {
        let row = app.descendants(matching: .any).matching(
            NSPredicate(format: "label CONTAINS[c] %@", session)).firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 20), "\(session) session not listed")
        row.tap()
        let frame = app.otherElements["terminal-frame"]
        XCTAssertTrue(frame.waitForExistence(timeout: 10), "terminal did not open")
        Thread.sleep(forTimeInterval: 2.0)
        for _ in 0..<3 where !app.keyboards.firstMatch.exists {
            frame.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.98)).tap()
            _ = app.keyboards.firstMatch.waitForExistence(timeout: 3)
        }
        XCTAssertTrue(app.keyboards.firstMatch.exists, "keyboard did not appear")
        Thread.sleep(forTimeInterval: 0.6)
        return frame
    }

    private func keyRowKey(_ label: String) -> XCUIElement {
        app.descendants(matching: .any)["key-row"].descendants(matching: .any).matching(
            NSPredicate(format: "label == %@", label)).firstMatch
    }

    /// A full-screen app scrolled up inside itself (alternate screen with
    /// mouse reporting, as Claude Code's full-screen mode) offers the jump to
    /// bottom, which the local scrollback position never would.
    func testFullScreenAppScrollOffersJumpToBottom() throws {
        let frame = openTerminal("fixture-worker")
        let esc = keyRowKey("Esc")
        XCTAssertTrue(esc.waitForExistence(timeout: 3), "key-row Esc missing")
        // The agent echoes its argument raw, so a key-row Esc in it makes a
        // real escape sequence.
        func emit(_ sequence: String) {
            app.typeText("echo ")
            esc.tap()
            app.typeText("\(sequence)\n")
            Thread.sleep(forTimeInterval: 0.5)
        }
        emit("[?1049h")
        app.typeText("mouseon\n")
        Thread.sleep(forTimeInterval: 1.0)
        defer {
            app.typeText("mouseoff\n")
            emit("[?1049l")
        }

        let jump = app.buttons["Scroll to bottom"]
        XCTAssertFalse(jump.exists, "jump offered on the alternate screen before any scroll")
        frame.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.2))
            .press(forDuration: 0.05, thenDragTo: frame.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.7)))
        XCTAssertTrue(jump.waitForExistence(timeout: 3), "scrolling the full-screen app up did not offer the jump")
        jump.tap()
        let gone = NSPredicate(format: "exists == false")
        XCTAssertEqual(XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: gone, object: jump)], timeout: 5),
            .completed, "the jump stayed after returning the app to its bottom")
    }

    /// Scrolling up through idle scrollback offers the jump to bottom. The
    /// position was only re-read when output arrived, so the jump appeared
    /// while an agent was busy and never once it went quiet.
    func testScrollingUpIdleScrollbackOffersJumpToBottom() throws {
        let frame = openTerminal("fixture-worker")
        app.typeText("mouseoff\n")
        app.typeText("lineout 200\n")
        Thread.sleep(forTimeInterval: 3.0) // output done: from here on, idle

        let jump = app.buttons["Scroll to bottom"]
        XCTAssertFalse(jump.exists, "jump offered while following the bottom")
        frame.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.15))
            .press(forDuration: 0.05, thenDragTo: frame.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.85)))
        XCTAssertTrue(jump.waitForExistence(timeout: 3), "scrolling up idle scrollback did not offer the jump")
        jump.tap()
        let gone = NSPredicate(format: "exists == false")
        XCTAssertEqual(XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: gone, object: jump)], timeout: 5),
            .completed, "the jump stayed after returning to the bottom")
    }
}
