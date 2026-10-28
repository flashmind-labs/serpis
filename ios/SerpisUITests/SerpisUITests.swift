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

    /// A chat on Claude Code (the test machine's stand-in claude): an edit it asks to make waits
    /// for you, and Allow lets it go on.
    func testAllowingWhatClaudeCodeAsks() throws {
        guard let address = env["SERPIS_TEST_ADDRESS"], let key = env["SERPIS_TEST_KEY"] else {
            throw XCTSkip("no test machine: run ios/test-machine.sh and pass its environment as TEST_RUNNER_*")
        }
        let app = XCUIApplication()
        let data = NSTemporaryDirectory() + "serpis-ui-\(UUID().uuidString)"
        app.launchArguments = ["-SerpisMachines", #"[{"id":"box","name":"box","address":"\#(address)"}]"#, "-SerpisDeviceKey", key, "-SerpisData", data]
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
        let composer = app.textFields["composer"]
        XCTAssertTrue(composer.waitForExistence(timeout: 20))

        // Its models are the machine's claude's.
        let model = app.buttons["Model"]
        XCTAssertTrue(model.waitForExistence(timeout: 10))
        model.tap()
        let fake = app.buttons["Claude Code · Fake 5"]
        XCTAssertTrue(fake.waitForExistence(timeout: 10), "the machine offers Claude Code")
        fake.tap()

        composer.tap()
        composer.typeText("please edit")
        app.buttons["Send"].tap()
        let approval = app.otherElements["approval"]
        XCTAssertTrue(app.buttons["Allow"].waitForExistence(timeout: 30), "the edit waits for you")
        XCTAssertTrue(app.buttons["Always"].exists, "always is offered")
        // The keyboard away, for the screen.
        app.scrollViews["transcript"].coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.15)).tap()
        shot("11-approval")
        app.buttons["Allow"].tap()
        XCTAssertTrue(app.staticTexts["Edited."].waitForExistence(timeout: 30), "allowed, it goes on")
        XCTAssertFalse(approval.exists)
        XCTAssertFalse(app.buttons["Allow"].exists, "and asks no more")
        shot("12-allowed")

        // Its questions come as a form: an option for one, several for the other.
        composer.tap()
        composer.typeText("a question")
        app.buttons["Send"].tap()
        let form = app.otherElements["questions"]
        XCTAssertTrue(form.waitForExistence(timeout: 30), "the questions wait for you")
        app.scrollViews["transcript"].coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.15)).tap()
        let send = form.buttons["Send answers"]
        XCTAssertFalse(send.isEnabled, "not before each has an answer")
        form.buttons["Blue"].tap()
        form.buttons["S"].tap()
        form.buttons["L"].tap()
        shot("13-questions")
        XCTAssertTrue(send.isEnabled)
        send.tap()
        let reached = app.staticTexts["You chose Blue in S, L."].waitForExistence(timeout: 30)
        shot("14-answered")
        if !reached, app.alerts.firstMatch.exists { XCTFail("refused: \(app.alerts.firstMatch.staticTexts.allElementsBoundByIndex.map(\.label))") }
        XCTAssertTrue(reached, "the answers reached it")
        XCTAssertFalse(form.exists)

        // Its permission mode shows above the composer, and switches from there.
        let mode = app.buttons["permission-mode"]
        if !mode.waitForExistence(timeout: 10) {
            XCTFail("the mode shows; on screen: \(app.debugDescription.components(separatedBy: "\n").filter { $0.contains("Button") || $0.contains("StaticText") }.suffix(40).joined(separator: " | "))")
        }
        mode.tap()
        // (A row with its description is labelled with both.)
        let auto = app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", "⏵⏵ auto mode on")).firstMatch
        XCTAssertTrue(auto.waitForExistence(timeout: 5))
        auto.tap()
        XCTAssertTrue(app.buttons["Permission mode: ⏵⏵ auto mode on"].waitForExistence(timeout: 20), "switched: \(mode.label)")

        // A plan to approve: its own card, approved accepting edits.
        mode.tap()
        let planMode = app.buttons.matching(NSPredicate(format: "label BEGINSWITH %@", "⏸ plan mode on")).firstMatch
        XCTAssertTrue(planMode.waitForExistence(timeout: 5))
        planMode.tap()
        composer.tap()
        composer.typeText("make a plan")
        app.buttons["Send"].tap()
        let plan = app.descendants(matching: .any)["plan"]
        XCTAssertTrue(plan.waitForExistence(timeout: 30), "the plan waits for approval")
        app.scrollViews["transcript"].coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.15)).tap()
        shot("14a-plan")
        let accept = app.buttons["Accept edits"]
        XCTAssertTrue(accept.waitForExistence(timeout: 5))
        accept.tap()
        XCTAssertTrue(app.staticTexts["Plan approved; on in acceptEdits."].waitForExistence(timeout: 30), "approved, it goes on")

        // While it thinks, the phone says so, with how much.
        composer.tap()
        composer.typeText("think it over for a while")
        app.buttons["Send"].tap()
        let thinking = app.descendants(matching: .any).matching(NSPredicate(format: "label BEGINSWITH 'Thinking, about'")).firstMatch
        XCTAssertTrue(thinking.waitForExistence(timeout: 20), "thinking shows")
        shot("14b-thinking")
        XCTAssertTrue(app.staticTexts["Thought it through."].waitForExistence(timeout: 30))

        // A subagent's turn.
        composer.tap()
        composer.typeText("use a subagent")
        app.buttons["Send"].tap()
        XCTAssertTrue(app.staticTexts["The subagent said from-sub."].waitForExistence(timeout: 30), "a subagent's turn")

        // A command left running: the phone says so above its composer, and stops it.
        composer.tap()
        composer.typeText("background forever")
        app.buttons["Send"].tap()
        let running = app.descendants(matching: .any)["background"]
        let shown = running.waitForExistence(timeout: 30)
        app.scrollViews["transcript"].coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.15)).tap()
        shot("15-background")
        XCTAssertTrue(shown, "what runs in the background shows")
        // Its card shows what it prints, live: opened from the turn's "Worked" row.
        let worked = app.buttons.matching(NSPredicate(format: "label BEGINSWITH 'Worked'")).allElementsBoundByIndex.last
        worked?.tap()
        let command = app.buttons.matching(NSPredicate(format: "label CONTAINS 'sleep 300'")).firstMatch
        if command.waitForExistence(timeout: 5) { command.tap() }
        // (Below the screen's edge, and the list makes only what is on screen.)
        app.scrollViews["transcript"].swipeUp()
        let printed = app.staticTexts.matching(NSPredicate(format: "label CONTAINS 'progress 100%'")).firstMatch
        let live = printed.waitForExistence(timeout: 15)
        shot("15a-background-output")
        if !live {
            XCTFail("its output shows; on screen: \(app.debugDescription.components(separatedBy: "\n").filter { $0.contains("Button") || $0.contains("StaticText") }.suffix(30).joined(separator: " | "))")
        }
        running.buttons["Stop"].tap()
        let item = app.buttons["sleep then echo bg-done"]
        XCTAssertTrue(item.waitForExistence(timeout: 5))
        item.tap()
        XCTAssertTrue(running.waitForNonExistence(timeout: 30), "stopped, it shows no more")

        // Close to a usage limit, it says how close; stopped at one, until when, and that it
        // goes on by itself then (or, told so, waits). Sent once the chat is idle, so it isn't
        // taken in by the turn still ending.
        XCTAssertTrue(app.buttons["Stop"].waitForNonExistence(timeout: 30), "the chat is idle")
        composer.tap()
        composer.typeText("near the limit")
        app.buttons["Send"].tap()
        let warning = app.staticTexts["limit-warning"]
        if !warning.waitForExistence(timeout: 30) {
            XCTFail("the warning shows; on screen: \(app.debugDescription.components(separatedBy: "\n").filter { $0.contains("Button") || $0.contains("StaticText") }.suffix(40).joined(separator: " | "))")
        }
        XCTAssertEqual(warning.label, "⚠ 85% of session limit")
        composer.tap()
        composer.typeText("hit the limit for an hour")
        app.buttons["Send"].tap()
        let wait = app.buttons["limit-wait"]
        let stopped = wait.waitForExistence(timeout: 30)
        app.scrollViews["transcript"].coordinate(withNormalizedOffset: CGVector(dx: 0.5, dy: 0.15)).tap()
        shot("16-limit")
        XCTAssertTrue(stopped, "the limit card shows, going on by itself")
        XCTAssertTrue(app.staticTexts["Session limit reached"].exists)
        wait.tap()
        XCTAssertTrue(app.buttons["limit-go"].waitForExistence(timeout: 20), "told not to, it waits for you")
        // The chat list says so too.
        app.navigationBars.buttons.element(boundBy: 0).tap()
        let row = app.staticTexts["row-limit"]
        let listed = row.waitForExistence(timeout: 20)
        shot("17-limit-list")
        XCTAssertTrue(listed, "its row says it stopped at the limit")
        XCTAssertEqual(row.label, "Session limit · waits for you")
    }
}
