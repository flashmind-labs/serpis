import XCTest

/// The phone promo's footage (scripts/promo/ios/record.sh): the app driven through its scenes
/// while the simulator's screen is recorded, against a demo machine (the test machine with the
/// scripted demo model and a demo project). Each scene start is written to SERPIS_PROMO_MARKS as
/// "<unix time> <name>". Skipped unless that is set.
final class PromoRecording: XCTestCase {
    private var env: [String: String] { ProcessInfo.processInfo.environment }
    private var marks: FileHandle?

    private func mark(_ name: String) {
        marks?.write("\(Date().timeIntervalSince1970) \(name)\n".data(using: .utf8)!)
    }

    /// Typed a word at a time (a character at a time takes XCUITest most of a second each).
    private func type(_ text: String, into field: XCUIElement) {
        let words = text.split(separator: " ", omittingEmptySubsequences: false)
        for (i, w) in words.enumerated() {
            field.typeText(String(w) + (i < words.count - 1 ? " " : ""))
            Thread.sleep(forTimeInterval: 0.05)
        }
    }

    private func pause(_ s: Double) { Thread.sleep(forTimeInterval: s) }

    func testPromo() throws {
        guard let path = env["SERPIS_PROMO_MARKS"], let address = env["SERPIS_TEST_ADDRESS"], let key = env["SERPIS_TEST_KEY"] else {
            throw XCTSkip("only for the promo recording (scripts/promo/ios/record.sh)")
        }
        FileManager.default.createFile(atPath: path, contents: nil)
        marks = FileHandle(forWritingAtPath: path)
        let app = XCUIApplication()
        let data = NSTemporaryDirectory() + "serpis-promo-\(UUID().uuidString)"
        app.launchArguments = ["-SerpisMachines", #"[{"id":"studio","name":"studio","address":"\#(address)"}]"#, "-SerpisDeviceKey", key, "-SerpisData", data]
        app.launch()
        let confirm = app.buttons["Confirm studio's key to reach it"]
        XCTAssertTrue(confirm.waitForExistence(timeout: 30))
        confirm.tap()
        app.buttons["Trust it"].tap()
        XCTAssertTrue(app.buttons["Trust it"].waitForNonExistence(timeout: 20))
        app.buttons["Done"].tap()
        let composer = app.textFields["composer"]

        // Before the footage: a few chats, so the list looks lived in.
        for prompt in ["Update the README for the 0.3 release", "Why is the release build slower?"] {
            app.buttons["new-chat"].tap()
            XCTAssertTrue(app.buttons["ratelimit"].waitForExistence(timeout: 20))
            app.buttons["ratelimit"].tap()
            app.buttons["Start a chat here"].tap()
            XCTAssertTrue(composer.waitForExistence(timeout: 20))
            composer.tap()
            composer.typeText(prompt)
            app.buttons["Send"].tap()
            XCTAssertTrue(app.buttons["abort"].waitForNonExistence(timeout: 30))
            app.navigationBars.buttons.element(boundBy: 0).tap()
        }
        XCTAssertTrue(app.buttons["chat-row"].waitForExistence(timeout: 20))
        pause(1.5)

        // 1. The chats, everywhere.
        mark("list")
        pause(3.0)

        // 2. A chat in a project on the computer.
        mark("ask")
        app.buttons["new-chat"].tap()
        XCTAssertTrue(app.buttons["ratelimit"].waitForExistence(timeout: 20))
        pause(0.8)
        app.buttons["ratelimit"].tap()
        pause(0.6)
        app.buttons["Start a chat here"].tap()
        XCTAssertTrue(composer.waitForExistence(timeout: 20))
        composer.tap()
        pause(0.5)
        type("The rate limiter test is flaky on CI. Find out why and fix it.", into: composer)
        pause(0.8)
        app.buttons["Send"].tap()
        // The keyboard away, to watch it work.
        pause(0.6)
        app.scrollViews["transcript"].coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.15)).tap()
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS 'no flakes'")).firstMatch.waitForExistence(timeout: 60))
        pause(2.0)

        // 3. What it did, opened down to the diff.
        mark("work")
        let work = app.buttons.matching(identifier: "work").allElementsBoundByIndex.last
        work?.tap()
        pause(1.2)
        let edit = app.buttons.matching(NSPredicate(format: "label CONTAINS 'limiter.rs' AND label CONTAINS '+'")).firstMatch
        if edit.waitForExistence(timeout: 5) { edit.tap() }
        pause(3.0)

        // 4. Claude Code asks; the phone answers.
        mark("approve")
        app.navigationBars.buttons.element(boundBy: 0).tap()
        app.buttons["new-chat"].tap()
        XCTAssertTrue(app.buttons["ratelimit"].waitForExistence(timeout: 20))
        app.buttons["ratelimit"].tap()
        app.buttons["Start a chat here"].tap()
        XCTAssertTrue(composer.waitForExistence(timeout: 20))
        app.buttons["Model"].tap()
        let opus = app.buttons["Claude Code · Opus 5.5"]
        XCTAssertTrue(opus.waitForExistence(timeout: 10))
        pause(0.6)
        opus.tap()
        composer.tap()
        type("Make refill safe if the clock steps back, then edit it.", into: composer)
        pause(0.6)
        app.buttons["Send"].tap()
        XCTAssertTrue(app.buttons["Allow"].waitForExistence(timeout: 30))
        app.scrollViews["transcript"].coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.15)).tap()
        pause(2.2)
        mark("allow")
        app.buttons["Allow"].tap()
        XCTAssertTrue(app.staticTexts.matching(NSPredicate(format: "label CONTAINS 'steps back anymore'")).firstMatch.waitForExistence(timeout: 30))
        pause(2.5)
        mark("end")
        try? marks?.close()
    }
}
