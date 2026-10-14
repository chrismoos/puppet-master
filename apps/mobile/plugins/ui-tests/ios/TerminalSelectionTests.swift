import XCTest

final class TerminalSelectionTests: XCTestCase {
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

    private func attachTree() {
        let tree = XCTAttachment(string: app.debugDescription)
        tree.name = "tree"
        tree.lifetime = .keepAlways
        add(tree)
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

    func testLongPressSelectsAndCopiesWhileOutputStreams() throws {
        let frame = openTerminal("fixture-planning")

        // Mouse reporting on and output streaming, as with an agent TUI
        // redrawing: the selection must survive both.
        app.typeText("mouseon\n")
        Thread.sleep(forTimeInterval: 0.5)
        // One unbroken word wide enough to wrap across the visible rows,
        // so the press below lands inside it.
        let word = String(repeating: "selectme", count: 60)
        app.typeText("echo \(word)\n")
        Thread.sleep(forTimeInterval: 1.0)
        app.typeText("pacedout 300 1\n")
        Thread.sleep(forTimeInterval: 0.5)

        frame.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.35)).press(forDuration: 0.8)
        // Output keeps arriving while the menu is up.
        Thread.sleep(forTimeInterval: 2.0)

        let copy = app.menuItems["Copy"].exists ? app.menuItems["Copy"] : app.buttons.matching(
            NSPredicate(format: "label == 'Copy' AND NOT (identifier BEGINSWITH 'key')")).firstMatch
        if !copy.waitForExistence(timeout: 3) { attachTree() }
        XCTAssertTrue(copy.exists, "long press did not offer Copy, or output cleared the selection")
        let keyRow = app.descendants(matching: .any)["key-row"]
        XCTAssertTrue(keyRow.descendants(matching: .any).matching(
            NSPredicate(format: "label == 'Copy'")).firstMatch.exists,
            "the key row did not learn of the selection")

        copy.tap()
        // Reading the pasteboard here would raise the system paste prompt in
        // the runner, so check what the app shows instead: the selection is
        // gone and the key row offers to paste what was copied.
        let paste = keyRow.descendants(matching: .any).matching(
            NSPredicate(format: "label == 'Paste'")).firstMatch
        XCTAssertTrue(paste.waitForExistence(timeout: 3), "copy did not fill the pasteboard")
        XCTAssertFalse(app.menuItems["Copy"].exists, "menu stayed up after copying")
    }

    func testDraggingAHandleExtendsTheSelection() throws {
        let frame = openTerminal("fixture-worker")
        app.typeText("mouseon\n")
        Thread.sleep(forTimeInterval: 0.5)
        // Short words across many rows, so the press selects one word and
        // its end handle sits right under the finger.
        app.typeText("echo \(Array(repeating: "abcd", count: 160).joined(separator: " "))\n")
        Thread.sleep(forTimeInterval: 1.0)

        let pressAt = frame.coordinate(withNormalizedOffset: CGVector(dx: 0.52, dy: 0.3))
        pressAt.press(forDuration: 0.8)
        Thread.sleep(forTimeInterval: 1.0)
        shot("after-press")
        XCTAssertTrue(app.menuItems["Copy"].waitForExistence(timeout: 3), "long press did not select")

        // A quick drag from the handle (shorter than the long press) must
        // move the selection's end, not scroll the terminal.
        pressAt.press(forDuration: 0.15,
                      thenDragTo: frame.coordinate(withNormalizedOffset: CGVector(dx: 0.8, dy: 0.5)))
        Thread.sleep(forTimeInterval: 1.0)
        shot("after-drag")
        XCTAssertTrue(app.menuItems["Copy"].waitForExistence(timeout: 3), "selection lost after dragging its handle")
    }

    private func shot(_ name: String) {
        let a = XCTAttachment(screenshot: app.screenshot())
        a.name = name
        a.lifetime = .keepAlways
        add(a)
    }
}
