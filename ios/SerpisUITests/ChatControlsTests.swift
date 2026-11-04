import XCTest

/// What a chat offers beyond sending: selecting part of what was said, and its thinking level.
/// Against the test machine (ios/test-machine.sh); skipped without one.
final class ChatControlsTests: XCTestCase {
    private var env: [String: String] { ProcessInfo.processInfo.environment }

    private func shot(_ name: String) {
        guard let dir = env["SERPIS_SHOTS"] else { return }
        Thread.sleep(forTimeInterval: 0.6)
        try? XCUIScreen.main.screenshot().pngRepresentation.write(to: URL(fileURLWithPath: dir).appendingPathComponent("\(name).png"))
    }

    /// The app, in a new chat on the test machine.
    private func newChat() throws -> XCUIApplication {
        guard let address = env["SERPIS_TEST_ADDRESS"], let key = env["SERPIS_TEST_KEY"] else {
            throw XCTSkip("no test machine: run ios/test-machine.sh and pass its environment as TEST_RUNNER_*")
        }
        let app = XCUIApplication()
        app.launchArguments = ["-SerpisMachines", #"[{"id":"box","name":"box","address":"\#(address)"}]"#, "-SerpisDeviceKey", key, "-SerpisData", NSTemporaryDirectory() + "serpis-ui-\(UUID().uuidString)"]
        app.launch()
        let confirm = app.buttons["Confirm box's key to reach it"]
        XCTAssertTrue(confirm.waitForExistence(timeout: 20))
        confirm.tap()
        let trust = app.buttons["Trust it"]
        XCTAssertTrue(trust.waitForExistence(timeout: 5))
        trust.tap()
        XCTAssertTrue(trust.waitForNonExistence(timeout: 20))
        app.buttons["Done"].tap()
        app.buttons["new-chat"].tap()
        XCTAssertTrue(app.buttons["project"].waitForExistence(timeout: 20))
        app.buttons["project"].tap()
        app.buttons["Start a chat here"].tap()
        XCTAssertTrue(app.textFields["composer"].waitForExistence(timeout: 20))
        return app
    }

    func testSelectingPartOfAnAnswer() throws {
        let app = try newChat()
        let composer = app.textFields["composer"]
        composer.tap()
        composer.typeText("SHOW me how it builds")
        app.buttons["Send"].tap()
        let answer = app.staticTexts["Steps"]
        XCTAssertTrue(answer.waitForExistence(timeout: 30), "the answer shows")
        app.scrollViews["transcript"].coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.15)).tap()

        answer.press(forDuration: 1)
        let select = app.buttons["Select Text"]
        XCTAssertTrue(select.waitForExistence(timeout: 5), "a long press offers it")
        XCTAssertTrue(app.buttons["Copy"].exists)
        shot("select-1-menu")
        select.tap()

        let text = app.textViews["selectable-text"]
        XCTAssertTrue(text.waitForExistence(timeout: 5), "it opens as text to select")
        let value = text.value as? String ?? ""
        XCTAssertTrue(value.contains("Fetch"), value)
        XCTAssertFalse(value.contains("**"), "without its markup: \(value)")
        XCTAssertTrue(value.contains("cargo build --release"), "code too")

        // A word, selected: part of it, with the system's menu to copy it.
        text.coordinate(withNormalizedOffset: CGVector(dx: 0.3, dy: 0.05)).doubleTap()
        XCTAssertTrue(app.menuItems["Copy"].waitForExistence(timeout: 5) || app.buttons["Copy"].waitForExistence(timeout: 1), "a selection, and Copy for it")
        shot("select-2-selection")
        app.buttons["Done"].tap()
        XCTAssertTrue(text.waitForNonExistence(timeout: 5))
    }

    func testSeeingAndChoosingHowMuchItThinks() throws {
        let app = try newChat()
        let model = app.buttons["Model"]
        XCTAssertTrue(model.waitForExistence(timeout: 10))
        // The mock model doesn't think: nothing to choose, and it says nothing of it.
        XCTAssertFalse((model.value as? String ?? "").contains("thinking"), "\(model.value ?? "")")

        model.tap()
        let fake = app.buttons["Claude Code · Fake 5"]
        XCTAssertTrue(fake.waitForExistence(timeout: 10))
        fake.tap()
        // (Off, as it was on the model that doesn't think.)
        let off = NSPredicate(format: "value CONTAINS 'thinking Off'")
        XCTAssertTrue(XCTNSPredicateExpectation(predicate: off, object: model).wait(), "it shows what the chat uses: \(model.value ?? "")")
        shot("thinking-1-shown")

        model.tap()
        let thinking = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Thinking'")).firstMatch
        XCTAssertTrue(thinking.waitForExistence(timeout: 5))
        shot("thinking-2-menu")
        thinking.tap()
        let high = app.buttons["High"]
        XCTAssertTrue(high.waitForExistence(timeout: 5))
        XCTAssertFalse(app.buttons["Minimal"].exists, "only what this model takes")
        shot("thinking-3-levels")
        high.tap()
        let chosen = NSPredicate(format: "value CONTAINS 'thinking High'")
        XCTAssertTrue(XCTNSPredicateExpectation(predicate: chosen, object: model).wait(), "the machine took it: \(model.value ?? "")")
        shot("thinking-4-chosen")
    }
}

private extension XCTNSPredicateExpectation {
    func wait() -> Bool { XCTWaiter().wait(for: [self], timeout: 10) == .completed }
}
