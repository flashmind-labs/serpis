import XCTest

/// The app against a machine made for it (ios/test-machine.sh, whose environment reaches the
/// runner as TEST_RUNNER_SERPIS_TEST_*): trusting its key, starting a chat in a folder there,
/// an answer, a tool, and the chat in the list. Screens go to SERPIS_SHOTS when it is set.
/// Skipped without a machine.
final class SerpisUITests: XCTestCase {
    private var env: [String: String] { ProcessInfo.processInfo.environment }

    private func shot(_ name: String) {
        guard let dir = env["SERPIS_SHOTS"] else { return }
        // Not mid-animation.
        Thread.sleep(forTimeInterval: 0.6)
        let url = URL(fileURLWithPath: dir).appendingPathComponent("\(name).png")
        try? XCUIScreen.main.screenshot().pngRepresentation.write(to: url)
    }

    /// Pairing with a computer: its code's link (the Camera app hands it over) brings the
    /// machine, trusted as the computer trusts it; the computer says yes (the test machine's
    /// stand-in desktop does at once), and this phone, let in with its own key, reaches it.
    func testPairingWithYourComputer() throws {
        guard let link = env["SERPIS_TEST_PAIR_LINK"], let key = env["SERPIS_TEST_PAIR_KEY"], let url = URL(string: link) else {
            throw XCTSkip("no test machine: run ios/test-machine.sh and pass its environment as TEST_RUNNER_*")
        }
        let app = XCUIApplication()
        let data = NSTemporaryDirectory() + "serpis-ui-\(UUID().uuidString)"
        app.launchArguments = ["-SerpisMachines", "[]", "-SerpisDeviceKey", key, "-SerpisData", data]
        app.launch()
        XCTAssertTrue(app.staticTexts["No machines yet"].waitForExistence(timeout: 20))
        shot("p1-no-machines")

        XCUIDevice.shared.system.open(url)
        // (iOS may ask before opening the app from a link.)
        let springboard = XCUIApplication(bundleIdentifier: "com.apple.springboard")
        if springboard.buttons["Open"].waitForExistence(timeout: 3) { springboard.buttons["Open"].tap() }
        XCTAssertTrue(app.staticTexts["Paired"].waitForExistence(timeout: 30), "the computer said yes")
        XCTAssertTrue(app.staticTexts["This phone can reach box."].exists)
        shot("p2-paired")
        app.buttons["Done"].tap()

        // Its host key came with the code (nothing to confirm), and this phone's key gets in.
        let reached = app.descendants(matching: .any)["chat-row"].waitForExistence(timeout: 30) || app.staticTexts["No chats yet"].exists
        XCTAssertTrue(reached, "the machine's chats show")
        XCTAssertFalse(app.buttons["Confirm box's key to reach it"].exists, "no host key to confirm")
        shot("p3-reached")
    }

    func testAChatOnAMachine() throws {
        guard let address = env["SERPIS_TEST_ADDRESS"], let key = env["SERPIS_TEST_KEY"] else {
            throw XCTSkip("no test machine: run ios/test-machine.sh and pass its environment as TEST_RUNNER_*")
        }
        let app = XCUIApplication()
        let data = NSTemporaryDirectory() + "serpis-ui-\(UUID().uuidString)"
        app.launchArguments = ["-SerpisMachines", #"[{"id":"box","name":"box","address":"\#(address)"}]"#, "-SerpisDeviceKey", key, "-SerpisData", data]
        app.launch()

        // A machine never reached: its key is asked about first.
        let confirm = app.buttons["Confirm box's key to reach it"]
        XCTAssertTrue(confirm.waitForExistence(timeout: 20))
        shot("1-unknown-host")
        confirm.tap()
        let trust = app.buttons["Trust it"]
        XCTAssertTrue(trust.waitForExistence(timeout: 5))
        XCTAssertTrue(app.staticTexts[env["SERPIS_TEST_FINGERPRINT"] ?? ""].exists, "the key shown is the machine's")
        shot("2-trust")
        trust.tap()
        XCTAssertTrue(trust.waitForNonExistence(timeout: 20), "trusted, it connects")
        shot("3-machines")
        app.buttons["Done"].tap()

        // A chat in a folder there.
        app.buttons["new-chat"].tap()
        let project = app.buttons["project"]
        XCTAssertTrue(project.waitForExistence(timeout: 20))
        shot("4-folders")
        project.tap()
        let start = app.buttons["Start a chat here"]
        XCTAssertTrue(start.waitForExistence(timeout: 10))
        start.tap()

        let composer = app.textFields["composer"]
        XCTAssertTrue(composer.waitForExistence(timeout: 20))
        shot("5-new-chat")
        composer.tap()
        composer.typeText("SHOW me how it builds")
        app.buttons["Send"].tap()
        XCTAssertTrue(app.staticTexts["Steps"].waitForExistence(timeout: 30), "the answer shows")
        shot("6-answer")

        composer.tap()
        composer.typeText(#"CALL bash {"command": "ls"}"#)
        app.buttons["Send"].tap()
        XCTAssertTrue(app.buttons["work"].waitForExistence(timeout: 30), "the tool shows, folded into its stretch of work")
        shot("7-tool")

        // A stretch of work, folded into one line, and opened.
        composer.tap()
        composer.typeText("WORK on the readme")
        app.buttons["Send"].tap()
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS 'updated the readme'")).firstMatch.waitForExistence(timeout: 30), "the answer after the work")
        // The keyboard away, for the screens.
        app.scrollViews["transcript"].coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.15)).tap()
        XCTAssertTrue(app.keyboards.firstMatch.waitForNonExistence(timeout: 5), "a tap on the transcript puts the keyboard away")
        shot("7b-work")
        let work = app.buttons.matching(identifier: "work").allElementsBoundByIndex.last!
        work.tap()
        let command = app.buttons.matching(NSPredicate(format: "label CONTAINS 'ls -la'")).firstMatch
        XCTAssertTrue(command.waitForExistence(timeout: 5), "its steps show")
        command.tap()
        let edit = app.buttons.matching(NSPredicate(format: "label CONTAINS 'README.md' AND label CONTAINS '+'")).firstMatch
        shot("7c-work-open")
        if edit.waitForExistence(timeout: 5) { edit.tap() }
        shot("7d-work-diff")

        // Back: the chat is in the list.
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.staticTexts["SHOW me how it builds"].waitForExistence(timeout: 20))
        shot("8-list")

        // A second chat goes to the top: the latest first.
        app.buttons["new-chat"].tap()
        XCTAssertTrue(app.buttons["project"].waitForExistence(timeout: 20))
        app.buttons["project"].tap()
        app.buttons["Start a chat here"].tap()
        XCTAssertTrue(composer.waitForExistence(timeout: 20))
        composer.tap()
        composer.typeText("the second one")
        app.buttons["Send"].tap()
        XCTAssertTrue(app.staticTexts["ok"].waitForExistence(timeout: 30))
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let rows = app.buttons.matching(identifier: "chat-row")
        XCTAssertTrue(rows.firstMatch.waitForExistence(timeout: 20))
        XCTAssertTrue(rows.element(boundBy: 0).label.contains("the second one"), "the latest first: \(rows.element(boundBy: 0).label)")

        // Quit and back: the chats are listed in the same order, and open.
        app.terminate()
        app.launch()
        let back = rows.firstMatch.waitForExistence(timeout: 30)
        shot("9-relaunched")
        XCTAssertTrue(back)
        XCTAssertTrue(rows.element(boundBy: 0).label.contains("the second one"), "after a restart too: \(rows.element(boundBy: 0).label)")
        XCTAssertTrue(rows.element(boundBy: 1).label.contains("SHOW me how it builds"))
        shot("9-after-restart")
        rows.element(boundBy: 0).tap()
        XCTAssertTrue(app.staticTexts["the second one"].waitForExistence(timeout: 20), "it opens, with what was said")
        shot("10-reopened")
    }
}
