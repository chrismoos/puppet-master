import XCTest

/// Geometric assertion: the focused custom-option field bottom is above
/// the sticky submit bar top, which is above the keyboard top.
///
/// Requires a UI-test build (PM_UI_TEST_BUILD=1) enrolled against a
/// controller-fixture daemon with an active plan decision that has
/// allow_custom=1.
final class PlanDecisionClearanceTests: XCTestCase {
    private var app: XCUIApplication!

    override func setUpWithError() throws {
        continueAfterFailure = false
        app = XCUIApplication()
        app.launch()

        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        let allowBtn = springboard.buttons["Allow"]
        if allowBtn.waitForExistence(timeout: 5) {
            let dontAllow = springboard.buttons["Don\u{2019}t Allow"]
            if dontAllow.exists {
                dontAllow.tap()
            } else {
                allowBtn.tap()
            }
            _ = app.wait(for: .runningForeground, timeout: 5)
        }
    }

    override func tearDownWithError() throws {
        let screenshot = XCUIScreen.main.screenshot()
        let attachment = XCTAttachment(screenshot: screenshot)
        attachment.name = "plan-decision-clearance"
        attachment.lifetime = .keepAlways
        add(attachment)
    }

    private func navigateToPlanDecision() -> Bool {
        let sessionText = app.staticTexts.matching(
            NSPredicate(format: "label CONTAINS[c] 'fixture-planning' OR label CONTAINS[c] 'Architecture'")
        ).firstMatch
        guard sessionText.waitForExistence(timeout: 20) else { return false }
        sessionText.tap()

        let moreOptions = app.buttons["More options"]
        guard moreOptions.waitForExistence(timeout: 10) else { return false }
        moreOptions.tap()

        let planItem = app.buttons.matching(
            NSPredicate(format: "label CONTAINS[c] 'needs input'")
        ).firstMatch
        guard planItem.waitForExistence(timeout: 5) else { return false }
        planItem.tap()

        return true
    }

    func testCustomOptionFieldNotClippedBySubmitBar() throws {
        _ = navigateToPlanDecision()

        let addYourOwn = app.switches["Add your own option"]
        XCTAssertTrue(
            addYourOwn.waitForExistence(timeout: 30),
            "Plan decision with custom option must be visible. "
            + "Verify the controller-fixture daemon is running and the "
            + "UI-test build enrolled successfully."
        )

        if addYourOwn.value as? String != "1" {
            addYourOwn.tap()
        }

        let customField = app.textViews["Custom option text"]
        XCTAssertTrue(
            customField.waitForExistence(timeout: 5),
            "Custom option input field should appear"
        )
        customField.tap()

        let keyboard = app.keyboards.firstMatch
        XCTAssertTrue(
            keyboard.waitForExistence(timeout: 5),
            "Software keyboard should appear when field is focused"
        )

        let submitButton = app.buttons["Submit decision"]
        let submitAll = app.buttons["Submit all decisions"]
        let barElement: XCUIElement
        if submitButton.exists {
            barElement = submitButton
        } else if submitAll.exists {
            barElement = submitAll
        } else {
            XCTFail("Neither 'Submit decision' nor 'Submit all decisions' button found")
            return
        }

        let fieldFrame = customField.frame
        let fieldBottom = fieldFrame.origin.y + fieldFrame.size.height

        let barFrame = barElement.frame
        let barTop = barFrame.origin.y

        let keyboardFrame = keyboard.frame
        let keyboardTop = keyboardFrame.origin.y

        let clearance = barTop - fieldBottom

        let measurements = """
        field bottom:     \(fieldBottom) pt
        submit bar top:   \(barTop) pt
        keyboard top:     \(keyboardTop) pt
        clearance:        \(clearance) pt
        """
        XCTContext.runActivity(named: "Measurements") { activity in
            let att = XCTAttachment(string: measurements)
            att.name = "geometric-measurements"
            att.lifetime = .keepAlways
            activity.add(att)
        }

        XCTAssertLessThanOrEqual(
            fieldBottom, barTop,
            "Field bottom (\(fieldBottom)) must be above submit bar top (\(barTop))."
        )
        XCTAssertLessThanOrEqual(
            barTop, keyboardTop,
            "Submit bar top (\(barTop)) must be above keyboard top (\(keyboardTop))."
        )
        XCTAssertGreaterThan(
            clearance, 0,
            "Positive clearance required between field bottom and submit bar top."
        )
    }
}
