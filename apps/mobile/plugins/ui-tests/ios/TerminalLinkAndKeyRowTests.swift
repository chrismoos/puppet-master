import XCTest

final class TerminalLinkAndKeyRowTests: XCTestCase {
    private var app: XCUIApplication!

    override func setUpWithError() throws {
        continueAfterFailure = false
        app = XCUIApplication()
        app.launch()
        XCTAssertTrue(app.wait(for: .runningForeground, timeout: 10),
            "app did not reach foreground")

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

    /// Opens a fixture session's terminal and focuses it, which brings up
    /// the keyboard and with it the key row.
    private func openTerminal(_ session: String) -> XCUIElement {
        let row = app.descendants(matching: .any).matching(
            NSPredicate(format: "label CONTAINS[c] %@", session)).firstMatch
        XCTAssertTrue(row.waitForExistence(timeout: 20), "\(session) session not listed")
        row.tap()

        let frame = app.otherElements["terminal-frame"]
        XCTAssertTrue(frame.waitForExistence(timeout: 10), "terminal did not open")
        Thread.sleep(forTimeInterval: 2.0)
        // Focus on the bottom row, the empty cursor line: a tap anywhere
        // else may land on a link an earlier run left in the scrollback.
        for _ in 0..<3 where !app.keyboards.firstMatch.exists {
            frame.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.98)).tap()
            _ = app.keyboards.firstMatch.waitForExistence(timeout: 3)
        }
        XCTAssertTrue(app.keyboards.firstMatch.exists, "keyboard did not appear")
        Thread.sleep(forTimeInterval: 0.6)
        return frame
    }

    func testKeyRowScrollsOnlySideways() throws {
        _ = openTerminal("fixture-planning")

        let keyRow = app.descendants(matching: .any)["key-row"]
        if !keyRow.waitForExistence(timeout: 5) {
            let tree = XCTAttachment(string: app.debugDescription)
            tree.name = "tree"
            tree.lifetime = .keepAlways
            add(tree)
        }
        XCTAssertTrue(keyRow.exists, "key row not shown")
        let labeled = { (label: String) in
            self.app.descendants(matching: .any).matching(NSPredicate(format: "label == %@", label)).firstMatch
        }
        XCTAssertTrue(labeled("Alt").exists, "Alt modifier missing from key row")
        let esc = labeled("Esc")
        XCTAssertTrue(esc.exists, "Esc key missing from key row")

        let rest = esc.frame
        let rowFrame = keyRow.frame
        XCTAssertGreaterThanOrEqual(rest.minY, rowFrame.minY - 0.5, "Esc label clipped above the row")
        XCTAssertLessThanOrEqual(rest.maxY, rowFrame.maxY + 0.5, "Esc label clipped below the row")

        keyRow.swipeUp()
        Thread.sleep(forTimeInterval: 0.8)
        XCTAssertEqual(esc.frame.minY, rest.minY, accuracy: 0.5, "key row scrolled vertically on swipe up")

        keyRow.swipeDown()
        Thread.sleep(forTimeInterval: 0.8)
        XCTAssertEqual(esc.frame.minY, rest.minY, accuracy: 0.5, "key row scrolled vertically on swipe down")

        // It still scrolls sideways to reach the keys past the edge.
        keyRow.swipeLeft()
        Thread.sleep(forTimeInterval: 0.8)
        XCTAssertLessThan(esc.frame.minX, rest.minX - 20, "key row did not scroll sideways")
        XCTAssertEqual(esc.frame.minY, rest.minY, accuracy: 0.5, "key row moved vertically on a sideways swipe")
    }

    func testTappingALinkOpensIt() throws {
        let frame = openTerminal("fixture-worker")

        // Mouse reporting on, as Claude Code and other full-screen agents
        // run, so a tap that misses the link becomes a mouse report.
        app.typeText("mouseon\n")
        Thread.sleep(forTimeInterval: 0.5)
        // Long enough to wrap across every visible row, so the tap below
        // lands on the link wherever the rows fall.
        let url = "https://example.com/" + String(repeating: "pm-link-tap-", count: 40)
        app.typeText("echo \(url)\n")
        Thread.sleep(forTimeInterval: 1.5)

        let safari = XCUIApplication(bundleIdentifier: "com.apple.mobilesafari")
        frame.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.75)).tap()
        XCTAssertTrue(safari.wait(for: .runningForeground, timeout: 10),
            "tapping a link in the terminal did not open it")
    }
}
