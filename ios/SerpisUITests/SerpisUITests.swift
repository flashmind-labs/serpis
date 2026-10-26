import XCTest

/// The app against a machine made for it (ios/test-machine.sh, whose environment reaches the
/// runner as TEST_RUNNER_SERPIS_TEST_*): trusting its key, starting a chat in a folder there,
/// an answer, a tool, and the chat in the list. Screens go to SERPIS_SHOTS when it is set.
/// Skipped without a machine.
final class SerpisUITests: XCTestCase {
    private var env: [String: String] { ProcessInfo.processInfo.environment }

    private func shot(_ name: String) {
        guard let dir = env["SERPIS_SHOTS"] else { return }
        let url = URL(fileURLWithPath: dir).appendingPathComponent("\(name).png")
        try? XCUIScreen.main.screenshot().pngRepresentation.write(to: url)
    }

    func testAChatOnAMachine() throws {
        guard let address = env["SERPIS_TEST_ADDRESS"], let key = env["SERPIS_TEST_KEY"] else {
            throw XCTSkip("no test machine: run ios/test-machine.sh and pass its environment as TEST_RUNNER_*")
        }
        let app = XCUIApplication()
        app.launchArguments = ["-SerpisMachines", #"[{"id":"box","name":"box","address":"\#(address)"}]"#, "-SerpisDeviceKey", key]
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
        XCTAssertTrue(app.buttons.matching(NSPredicate(format: "label CONTAINS 'Ran'")).firstMatch.waitForExistence(timeout: 30), "the tool shows as a line")
        shot("7-tool")

        // Back: the chat is in the list.
        app.navigationBars.buttons.element(boundBy: 0).tap()
        XCTAssertTrue(app.staticTexts["SHOW me how it builds"].waitForExistence(timeout: 20))
        shot("8-list")
    }
}
