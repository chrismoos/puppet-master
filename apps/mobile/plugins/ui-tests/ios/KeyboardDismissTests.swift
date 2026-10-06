import XCTest

final class KeyboardDismissTests: XCTestCase {
    private var app: XCUIApplication!
    private let tolerance: CGFloat = 1.0

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

    private func record(_ label: String, _ f: CGRect) {
        XCTContext.runActivity(named: label) { a in
            let s = "x=\(f.origin.x) y=\(f.origin.y) w=\(f.size.width) h=\(f.size.height)"
            let att = XCTAttachment(string: s)
            att.name = label
            att.lifetime = .keepAlways
            a.add(att)
        }
    }

    private func screenshot(_ name: String) {
        let att = XCTAttachment(screenshot: XCUIScreen.main.screenshot())
        att.name = name
        att.lifetime = .keepAlways
        add(att)
    }

    private func dismissKeyboard() {
        let kb = app.keyboards.firstMatch
        guard kb.exists else { return }
        let gone = NSPredicate(format: "exists == false")
        for key in ["return", "Return", "done", "Done", "go", "Go"] {
            let btn = kb.buttons[key]
            if btn.exists {
                btn.tap()
                if XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: gone, object: kb)], timeout: 3) == .completed {
                    Thread.sleep(forTimeInterval: 0.4); return
                }
                break
            }
        }
        if kb.exists {
            app.coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.01)).tap()
            if XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: gone, object: kb)], timeout: 3) == .completed {
                Thread.sleep(forTimeInterval: 0.4); return
            }
        }
        if kb.exists {
            kb.swipeDown()
            XCTAssertEqual(
                XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: gone, object: kb)], timeout: 3), .completed,
                "keyboard still visible after dismiss")
        }
        Thread.sleep(forTimeInterval: 0.4)
    }

    func testKeyboardDismissRestoresElementGeometry() throws {
        Thread.sleep(forTimeInterval: 1.0)

        var field: XCUIElement
        var anchor: XCUIElement

        let tf = app.textFields.firstMatch
        if tf.waitForExistence(timeout: 3) {
            field = tf
            anchor = app.staticTexts.matching(NSPredicate(format: "label == 'Controller'")).firstMatch
            XCTAssertTrue(anchor.waitForExistence(timeout: 2), "anchor not found")
        } else {
            let more = app.buttons["More options"]
            XCTAssertTrue(more.waitForExistence(timeout: 10), "sessions screen not reached")
            more.tap()
            let menuItem = app.descendants(matching: .any).matching(
                NSPredicate(format: "label ==[c] 'Settings'")).firstMatch
            XCTAssertTrue(menuItem.waitForExistence(timeout: 5), "Settings menu item not found")
            menuItem.tap()

            let back = app.buttons["Back"]
            XCTAssertTrue(back.waitForExistence(timeout: 5), "Settings screen did not load")

            field = app.textFields.firstMatch
            XCTAssertTrue(field.waitForExistence(timeout: 8), "no input on Settings")
            anchor = app.staticTexts.matching(NSPredicate(format: "label == 'Controller'")).firstMatch
            XCTAssertTrue(anchor.waitForExistence(timeout: 2), "anchor not found")
        }

        let anchorRest = anchor.frame
        let fieldRest = field.frame
        screenshot("rest")
        record("rest-anchor", anchorRest)
        record("rest-field", fieldRest)

        for i in 1...2 {
            let tag = "cycle\(i)"

            field.tap()
            let kb = app.keyboards.firstMatch
            XCTAssertTrue(kb.waitForExistence(timeout: 5), "\(tag): keyboard did not appear")
            Thread.sleep(forTimeInterval: 0.4)

            let kbFrame = kb.frame
            let fieldShown = field.frame
            let anchorShown = anchor.frame
            screenshot("\(tag)-shown")
            record("\(tag)-shown-keyboard", kbFrame)
            record("\(tag)-shown-field", fieldShown)
            record("\(tag)-shown-anchor", anchorShown)

            let fieldBottom = fieldShown.origin.y + fieldShown.size.height
            XCTAssertLessThanOrEqual(fieldBottom, kbFrame.origin.y + tolerance,
                "\(tag): field bottom (\(fieldBottom)) must clear keyboard top (\(kbFrame.origin.y))")

            dismissKeyboard()

            let anchorDismissed = anchor.frame
            let fieldDismissed = field.frame
            screenshot("\(tag)-dismissed")
            record("\(tag)-dismissed-anchor", anchorDismissed)
            record("\(tag)-dismissed-field", fieldDismissed)

            XCTAssertEqual(anchorDismissed.origin.y, anchorRest.origin.y, accuracy: tolerance,
                "\(tag): anchor y \(anchorDismissed.origin.y) != rest \(anchorRest.origin.y)")
            XCTAssertEqual(anchorDismissed.size.height, anchorRest.size.height, accuracy: tolerance,
                "\(tag): anchor h \(anchorDismissed.size.height) != rest \(anchorRest.size.height)")
            XCTAssertEqual(fieldDismissed.origin.y, fieldRest.origin.y, accuracy: tolerance,
                "\(tag): field y \(fieldDismissed.origin.y) != rest \(fieldRest.origin.y)")
        }
    }
}
