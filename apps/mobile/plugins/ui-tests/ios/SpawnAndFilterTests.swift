import XCTest

final class SpawnAndFilterTests: XCTestCase {
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

    private func element(beginningWith prefix: String) -> XCUIElement {
        app.descendants(matching: .any).matching(NSPredicate(format: "label BEGINSWITH %@", prefix)).firstMatch
    }

    private func openOverflow(_ item: String) {
        let more = app.buttons["More options"]
        XCTAssertTrue(more.waitForExistence(timeout: 20), "sessions screen not reached")
        more.tap()
        let entry = element(item)
        XCTAssertTrue(entry.waitForExistence(timeout: 5), "\(item) missing from the overflow menu")
        entry.tap()
    }

    func testAgentPickerListsEveryAgentAndSpawnOpensTheSession() throws {
        openOverflow("New session")

        // The sheet preselects a project when there is only one.
        let project = element("Select\u{2026}")
        if project.waitForExistence(timeout: 3) {
            project.tap()
            let fixtureProject = element("controller-fixture project")
            XCTAssertTrue(fixtureProject.waitForExistence(timeout: 5), "fixture project not offered")
            fixtureProject.tap()
        }
        if !element(beginningWith: "Inherit (").waitForExistence(timeout: 5) {
            let tree = XCTAttachment(string: app.debugDescription)
            tree.name = "tree"
            tree.lifetime = .keepAlways
            add(tree)
        }

        let agentField = element(beginningWith: "Inherit (")
        XCTAssertTrue(agentField.waitForExistence(timeout: 5), "agent picker missing")
        agentField.tap()
        for agent in ["Claude", "Codex", "Gemini", "OpenCode", "Antigravity"] {
            XCTAssertTrue(element(agent).waitForExistence(timeout: 3), "\(agent) missing from the agent picker")
        }
        // The fixture's agent shim answers as codex.
        element("Codex").tap()

        let prompt = app.textViews.firstMatch.exists ? app.textViews.firstMatch : app.textFields.firstMatch
        XCTAssertTrue(prompt.waitForExistence(timeout: 3), "prompt field missing")
        prompt.tap()
        prompt.typeText("spawned by the ui test")

        let spawn = element("Spawn")
        XCTAssertTrue(spawn.exists, "Spawn button missing")
        spawn.tap()

        XCTAssertTrue(app.otherElements["terminal-frame"].waitForExistence(timeout: 20),
            "the spawned session did not open")
    }

    func testClearingFiltersHidesTheFilterBar() throws {
        openOverflow("Filter")

        let bucketLabel = element("Bucket")
        XCTAssertTrue(bucketLabel.waitForExistence(timeout: 5), "filter bar did not open")
        let bucket = element("controller-fixture")
        XCTAssertTrue(bucket.waitForExistence(timeout: 5), "fixture bucket chip missing")
        bucket.tap()

        let clear = element("Clear filters")
        XCTAssertTrue(clear.waitForExistence(timeout: 5), "Clear filters did not appear")
        clear.tap()

        let gone = NSPredicate(format: "exists == false")
        XCTAssertEqual(XCTWaiter.wait(for: [XCTNSPredicateExpectation(predicate: gone, object: bucketLabel)], timeout: 3),
            .completed, "filter bar stayed open after clearing")
    }
}
