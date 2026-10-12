import XCTest

final class SessionKillResumeTests: XCTestCase {
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

    private func element(_ label: String) -> XCUIElement {
        app.descendants(matching: .any).matching(NSPredicate(format: "label == %@", label)).firstMatch
    }

    private func element(containing text: String) -> XCUIElement {
        app.descendants(matching: .any).matching(NSPredicate(format: "label CONTAINS %@", text)).firstMatch
    }

    private func waitGone(_ element: XCUIElement, _ message: String) {
        let gone = NSPredicate(format: "exists == false")
        XCTAssertEqual(XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: gone, object: element)], timeout: 10),
            .completed, message)
    }

    /// Spawns a promptless session, which can always restart once ended.
    private func spawnSession() {
        let more = app.buttons["More options"]
        XCTAssertTrue(more.waitForExistence(timeout: 20), "sessions screen not reached")
        more.tap()
        element("New session").tap()
        let project = element("Select\u{2026}")
        if project.waitForExistence(timeout: 3) {
            project.tap()
            element("controller-fixture project").tap()
        }
        let spawn = element("Spawn")
        XCTAssertTrue(spawn.waitForExistence(timeout: 5), "Spawn button missing")
        spawn.tap()
        XCTAssertTrue(app.otherElements["terminal-frame"].waitForExistence(timeout: 20),
            "the spawned session did not open")
    }

    func testKillReturnsToTheListWhereTheEndedSessionResumes() throws {
        spawnSession()

        app.buttons["More options"].tap()
        XCTAssertTrue(element("Kill session").waitForExistence(timeout: 5), "Kill session missing")
        XCTAssertFalse(element("Interrupt").exists, "Interrupt is still offered")
        element("Kill session").tap()
        let confirm = app.alerts.buttons["Kill"]
        XCTAssertTrue(confirm.waitForExistence(timeout: 5), "kill confirmation missing")
        confirm.tap()

        waitGone(app.otherElements["terminal-frame"], "kill did not return to the session list")
        let endedRow = element(containing: "ended ")
        XCTAssertTrue(endedRow.waitForExistence(timeout: 10), "the killed session left the list")
        endedRow.tap()

        let banner = app.descendants(matching: .any)["session-ended-banner"]
        XCTAssertTrue(banner.waitForExistence(timeout: 10), "ended session did not offer resume")
        let restart = element("Restart session").exists ? element("Restart session") : element("Resume conversation")
        XCTAssertTrue(restart.exists, "resume button missing")
        restart.tap()
        waitGone(banner, "the session did not come back after resume")
        XCTAssertTrue(app.otherElements["terminal-frame"].exists, "terminal closed after resume")
    }
}
